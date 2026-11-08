//! Offer/answer/ICE state machine and the capture-side session wrapper.
//!
//! ```text
//! Host:   Idle --offer--> OfferSent --answer--> Connected
//! Viewer: Idle --offer--> Connected --pause--> Paused --resume--> Connected
//!         any non-terminal state --stop/failed--> Stopped/Failed
//! ```
//!
//! The state machine never queues input. Pausing, locking and reconnecting
//! withdraw the controller lease in [`HostSession`], so resuming cannot replay
//! a pointer or keyboard batch from before the gap.

use crate::capability::{IceCandidate, MediaCapabilities, MediaSessionId, Sdp};
use crate::error::{GrantError, InputDenial, SignallingError};
use crate::grant::{ControlGrant, ControllerLease, GrantAuthority, GrantId, ViewGrant};
use crate::limits::{MAX_MEDIA_SESSION_MS, SIGNALLING_VERSION};
use crate::wire::{FailureReason, PauseReason, SignallingMessage, StopReason};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use ukk_protocol::{DeviceId, DomainId, LeaseId};

/// Authenticated remote viewer identity bound to one [`HostSession`].
///
/// The transport must pass the same pair it derived from the authenticated
/// device handshake; a signalling envelope itself never carries identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerBinding {
    pub device_id: DeviceId,
    pub session_id: DomainId,
}

impl ViewerBinding {
    #[must_use]
    pub fn new(device_id: DeviceId, session_id: DomainId) -> Self {
        Self {
            device_id,
            session_id,
        }
    }
}

/// Process-wide tombstone set for media session ids.
///
/// A Host owns one instance and passes clones to every [`HostSession`]; a
/// stopped id can then never be resurrected by constructing a new wrapper.
/// No global mutable state is used.
#[derive(Debug, Clone, Default)]
pub struct StoppedSessions {
    stopped: Arc<Mutex<BTreeSet<MediaSessionId>>>,
}

impl StoppedSessions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_stopped(&self, media_session_id: &MediaSessionId) -> bool {
        self.guard().contains(media_session_id)
    }

    /// Marks an id terminal. Returns `true` only for the first stop.
    pub fn mark_stopped(&self, media_session_id: &MediaSessionId) -> bool {
        self.guard().insert(media_session_id.clone())
    }

    fn guard(&self) -> MutexGuard<'_, BTreeSet<MediaSessionId>> {
        self.stopped
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Which side of the media session this endpoint represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Capture-side Host. It starts the offer and owns grants.
    Host,
    /// Viewing peer. It answers offers and receives media.
    Viewer,
}

/// Explicit media session state. `Stopped` and `Failed` are terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaState {
    Idle,
    OfferSent,
    Connected,
    Paused(PauseReason),
    Stopped(StopReason),
    Failed(FailureReason),
}

impl MediaState {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::OfferSent => "offer_sent",
            Self::Connected => "connected",
            Self::Paused(_) => "paused",
            Self::Stopped(_) => "stopped",
            Self::Failed(_) => "failed",
        }
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped(_) | Self::Failed(_))
    }
}

/// One media session state machine.
///
/// This type has no clock, no socket and no capture backend. The caller passes
/// absolute `now_ms` and delivers decoded [`SignallingMessage`] values.
#[derive(Debug, Clone)]
pub struct MediaSession {
    role: Role,
    media_session_id: MediaSessionId,
    expires_at_ms: Option<i64>,
    local_capabilities: MediaCapabilities,
    local_sdp: Sdp,
    state: MediaState,
    locked: bool,
    negotiated: Option<MediaCapabilities>,
}

impl MediaSession {
    pub fn host(
        media_session_id: MediaSessionId,
        expires_at_ms: i64,
        local_capabilities: MediaCapabilities,
        local_sdp: Sdp,
    ) -> Result<Self, SignallingError> {
        crate::capability::validate_expiry(expires_at_ms)?;
        local_capabilities.validate()?;
        Ok(Self {
            role: Role::Host,
            media_session_id,
            expires_at_ms: Some(expires_at_ms),
            local_capabilities,
            local_sdp,
            state: MediaState::Idle,
            locked: false,
            negotiated: None,
        })
    }

    pub fn viewer(
        media_session_id: MediaSessionId,
        local_capabilities: MediaCapabilities,
        local_sdp: Sdp,
    ) -> Result<Self, SignallingError> {
        local_capabilities.validate()?;
        Ok(Self {
            role: Role::Viewer,
            media_session_id,
            expires_at_ms: None,
            local_capabilities,
            local_sdp,
            state: MediaState::Idle,
            locked: false,
            negotiated: None,
        })
    }

    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    #[must_use]
    pub fn media_session_id(&self) -> &MediaSessionId {
        &self.media_session_id
    }

    #[must_use]
    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    #[must_use]
    pub fn state(&self) -> &MediaState {
        &self.state
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    #[must_use]
    pub fn negotiated(&self) -> Option<&MediaCapabilities> {
        self.negotiated.as_ref()
    }

    #[must_use]
    pub fn local_capabilities(&self) -> &MediaCapabilities {
        &self.local_capabilities
    }

    #[must_use]
    pub fn local_sdp(&self) -> &Sdp {
        &self.local_sdp
    }

    #[must_use]
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms
            .is_some_and(|expires_at_ms| now_ms >= expires_at_ms)
    }

    /// Capture-side Host creates the initial offer.
    pub fn start(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host starts an offer",
            ));
        }
        if self.state != MediaState::Idle {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        let expires_at_ms = self.expires_at_ms.ok_or(SignallingError::NotConnected)?;
        self.ensure_live(now_ms)?;
        if expires_at_ms > now_ms.saturating_add(MAX_MEDIA_SESSION_MS) {
            return Err(SignallingError::Invalid(
                "media session expiry exceeds the maximum lifetime",
            ));
        }
        let message = SignallingMessage::Offer {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
            expires_at_ms,
            capabilities: self.local_capabilities.clone(),
            sdp: self.local_sdp.clone(),
        };
        message.validate_at(now_ms)?;
        self.state = MediaState::OfferSent;
        Ok(message)
    }

    /// Handles one inbound signalling message. `Ok(Some(answer))` is returned
    /// only when a viewer accepts an offer; all other accepted messages return
    /// `Ok(None)`.
    pub fn handle(
        &mut self,
        message: &SignallingMessage,
        now_ms: i64,
    ) -> Result<Option<SignallingMessage>, SignallingError> {
        message.validate()?;
        if self.state.is_terminal() {
            return Err(SignallingError::Terminal);
        }
        if message.media_session_id() != &self.media_session_id {
            return Err(SignallingError::WrongSession);
        }
        match message {
            SignallingMessage::Stop { reason, .. } => {
                self.state = MediaState::Stopped(*reason);
                return Ok(None);
            }
            SignallingMessage::Failed { reason, .. } => {
                self.state = MediaState::Failed(*reason);
                return Ok(None);
            }
            _ => {}
        }
        self.ensure_live(now_ms)?;
        match self.role {
            Role::Host => self.handle_host(message, now_ms),
            Role::Viewer => self.handle_viewer(message, now_ms),
        }
    }

    fn handle_host(
        &mut self,
        message: &SignallingMessage,
        now_ms: i64,
    ) -> Result<Option<SignallingMessage>, SignallingError> {
        match (&self.state, message) {
            (MediaState::Idle, SignallingMessage::Offer { .. }) => Err(SignallingError::WrongRole(
                "a Host does not accept an offer from a viewer",
            )),
            (MediaState::Idle, _) => Err(SignallingError::UnexpectedMessage("idle")),
            (
                MediaState::OfferSent,
                SignallingMessage::Answer {
                    expires_at_ms,
                    capabilities,
                    ..
                },
            ) => {
                message.validate_at(now_ms)?;
                if Some(*expires_at_ms) != self.expires_at_ms {
                    return Err(SignallingError::ExpiryMismatch);
                }
                if !capabilities.is_answer_subset_of(&self.local_capabilities) {
                    return Err(SignallingError::InvalidAnswer(
                        "answer codecs/transports are not a subset of the offer",
                    ));
                }
                self.negotiated = Some(capabilities.clone());
                self.state = MediaState::Connected;
                Ok(None)
            }
            (MediaState::OfferSent, SignallingMessage::IceCandidate { .. })
            | (MediaState::OfferSent, SignallingMessage::IceComplete { .. }) => Ok(None),
            (MediaState::OfferSent, _) => {
                Err(SignallingError::UnexpectedMessage(self.state.name()))
            }
            (MediaState::Connected, SignallingMessage::IceCandidate { .. })
            | (MediaState::Connected, SignallingMessage::IceComplete { .. }) => Ok(None),
            (MediaState::Connected, _) => {
                Err(SignallingError::UnexpectedMessage(self.state.name()))
            }
            (MediaState::Paused(_), SignallingMessage::Resume { .. }) => Err(
                SignallingError::WrongRole("a viewer cannot resume a Host media session"),
            ),
            (MediaState::Paused(_), _) => {
                Err(SignallingError::UnexpectedMessage(self.state.name()))
            }
            (MediaState::Stopped(_), _) | (MediaState::Failed(_), _) => {
                Err(SignallingError::Terminal)
            }
        }
    }

    fn handle_viewer(
        &mut self,
        message: &SignallingMessage,
        now_ms: i64,
    ) -> Result<Option<SignallingMessage>, SignallingError> {
        match (&self.state, message) {
            (
                MediaState::Idle,
                SignallingMessage::Offer {
                    expires_at_ms,
                    capabilities,
                    ..
                },
            ) => {
                message.validate_at(now_ms)?;
                let negotiated = self.local_capabilities.intersect(capabilities)?;
                self.expires_at_ms = Some(*expires_at_ms);
                self.negotiated = Some(negotiated.clone());
                self.state = MediaState::Connected;
                let answer = SignallingMessage::Answer {
                    v: SIGNALLING_VERSION,
                    media_session_id: self.media_session_id.clone(),
                    expires_at_ms: *expires_at_ms,
                    capabilities: negotiated.without_ice_servers(),
                    sdp: self.local_sdp.clone(),
                };
                answer.validate_at(now_ms)?;
                Ok(Some(answer))
            }
            (MediaState::Idle, _) => Err(SignallingError::UnexpectedMessage("idle")),
            (MediaState::Connected, SignallingMessage::IceCandidate { .. })
            | (MediaState::Connected, SignallingMessage::IceComplete { .. }) => Ok(None),
            (MediaState::Connected, SignallingMessage::Pause { reason, .. }) => {
                self.state = MediaState::Paused(*reason);
                Ok(None)
            }
            (MediaState::Paused(_), SignallingMessage::Pause { reason, .. }) => {
                self.state = MediaState::Paused(*reason);
                Ok(None)
            }
            (MediaState::Paused(_), SignallingMessage::Resume { .. }) => {
                self.state = MediaState::Connected;
                Ok(None)
            }
            (MediaState::Connected, _) | (MediaState::Paused(_), _) => {
                Err(SignallingError::UnexpectedMessage(self.state.name()))
            }
            (MediaState::OfferSent, _) => Err(SignallingError::WrongRole(
                "a viewer never enters offer_sent",
            )),
            (MediaState::Stopped(_), _) | (MediaState::Failed(_), _) => {
                Err(SignallingError::Terminal)
            }
        }
    }

    /// Trickles one ICE candidate. Allowed for both roles after the offer has
    /// been sent/received and while connected.
    pub fn add_ice_candidate(
        &mut self,
        now_ms: i64,
        candidate: IceCandidate,
    ) -> Result<SignallingMessage, SignallingError> {
        if !matches!(self.state, MediaState::OfferSent | MediaState::Connected) {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        self.ensure_live(now_ms)?;
        let message = SignallingMessage::IceCandidate {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
            candidate,
        };
        message.validate()?;
        Ok(message)
    }

    /// Signals that no more ICE candidates will be sent.
    pub fn ice_complete(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        if !matches!(self.state, MediaState::OfferSent | MediaState::Connected) {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        self.ensure_live(now_ms)?;
        let message = SignallingMessage::IceComplete {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
        };
        message.validate()?;
        Ok(message)
    }

    /// Host pauses media and notifies the viewer.
    pub fn pause(
        &mut self,
        now_ms: i64,
        reason: PauseReason,
    ) -> Result<SignallingMessage, SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host pauses media",
            ));
        }
        if self.state != MediaState::Connected {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        self.ensure_live(now_ms)?;
        self.state = MediaState::Paused(reason);
        if reason == PauseReason::ScreenLocked {
            self.locked = true;
        }
        let message = SignallingMessage::Pause {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
            reason,
        };
        message.validate()?;
        Ok(message)
    }

    /// Host resumes media. A locked screen must be explicitly unlocked first.
    pub fn resume(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host resumes media",
            ));
        }
        if self.state.is_terminal() {
            return Err(SignallingError::Terminal);
        }
        if self.locked {
            return Err(SignallingError::Locked);
        }
        if !matches!(self.state, MediaState::Paused(_)) {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        self.ensure_live(now_ms)?;
        self.state = MediaState::Connected;
        let message = SignallingMessage::Resume {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
        };
        message.validate()?;
        Ok(message)
    }

    /// Locks the screen, pausing media and keeping it paused after unlock.
    pub fn lock(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host observes a screen lock",
            ));
        }
        if self.state.is_terminal() {
            return Err(SignallingError::Terminal);
        }
        if matches!(self.state, MediaState::Idle | MediaState::OfferSent) {
            return Err(SignallingError::NotConnected);
        }
        self.ensure_live(now_ms)?;
        self.locked = true;
        self.state = MediaState::Paused(PauseReason::ScreenLocked);
        let message = SignallingMessage::Pause {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
            reason: PauseReason::ScreenLocked,
        };
        message.validate()?;
        Ok(message)
    }

    /// Clears the lock but deliberately does not resume. The Host must call
    /// [`Self::resume`] after the local user is ready.
    pub fn unlock(&mut self) -> Result<(), SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host clears a screen lock",
            ));
        }
        if self.state.is_terminal() {
            return Err(SignallingError::Terminal);
        }
        self.locked = false;
        Ok(())
    }

    /// Suspends media for a transport reconnect. The viewer is told to pause;
    /// the caller must withdraw the controller lease.
    pub fn suspend_for_reconnect(
        &mut self,
        now_ms: i64,
    ) -> Result<SignallingMessage, SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host suspends for reconnect",
            ));
        }
        if self.state != MediaState::Connected {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        self.ensure_live(now_ms)?;
        self.state = MediaState::Paused(PauseReason::Reconnecting);
        let message = SignallingMessage::Pause {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
            reason: PauseReason::Reconnecting,
        };
        message.validate()?;
        Ok(message)
    }

    /// Resumes a session suspended for reconnect. Input from before the pause
    /// has no lease and is refused.
    pub fn resume_after_reconnect(
        &mut self,
        now_ms: i64,
    ) -> Result<SignallingMessage, SignallingError> {
        if self.role != Role::Host {
            return Err(SignallingError::WrongRole(
                "only the capture-side Host resumes after reconnect",
            ));
        }
        if !matches!(self.state, MediaState::Paused(PauseReason::Reconnecting)) {
            return Err(SignallingError::UnexpectedMessage(self.state.name()));
        }
        self.ensure_live(now_ms)?;
        self.state = MediaState::Connected;
        let message = SignallingMessage::Resume {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
        };
        message.validate()?;
        Ok(message)
    }

    /// Explicit stop. Terminal for this media session id.
    pub fn stop(&mut self, reason: StopReason) -> Result<SignallingMessage, SignallingError> {
        if self.state.is_terminal() {
            return Err(SignallingError::Terminal);
        }
        self.state = MediaState::Stopped(reason);
        let message = SignallingMessage::Stop {
            v: SIGNALLING_VERSION,
            media_session_id: self.media_session_id.clone(),
            reason,
        };
        message.validate()?;
        Ok(message)
    }

    /// Marks the session failed without sending a message.
    pub fn fail(&mut self, reason: FailureReason) {
        if !self.state.is_terminal() {
            self.state = MediaState::Failed(reason);
        }
    }

    fn ensure_live(&mut self, now_ms: i64) -> Result<(), SignallingError> {
        if self.is_expired_at(now_ms) {
            self.state = MediaState::Failed(FailureReason::Expired);
            return Err(SignallingError::Expired);
        }
        Ok(())
    }
}

impl fmt::Display for MediaSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "media session {} ({:?}) in {}",
            self.media_session_id,
            self.role,
            self.state.name()
        )
    }
}

/// Capture-side wrapper that binds the state machine to one authenticated
/// viewer and the grant/lease authority.
///
/// It is the only place that admits remote input. The viewer side has no such
/// method, and a viewer device that holds only a [`ViewGrant`] is refused by
/// the authority. A live view grant for the bound device and Session is
/// required before `start` and before accepting an answer or ICE.
#[derive(Debug)]
pub struct HostSession {
    media: MediaSession,
    grants: GrantAuthority,
    viewer: ViewerBinding,
    stopped: StoppedSessions,
}

impl HostSession {
    pub fn new(
        media_session_id: MediaSessionId,
        viewer: ViewerBinding,
        expires_at_ms: i64,
        local_capabilities: MediaCapabilities,
        local_sdp: Sdp,
        stopped: StoppedSessions,
    ) -> Result<Self, SignallingError> {
        if stopped.is_stopped(&media_session_id) {
            return Err(SignallingError::Terminal);
        }
        Ok(Self {
            media: MediaSession::host(
                media_session_id,
                expires_at_ms,
                local_capabilities,
                local_sdp,
            )?,
            grants: GrantAuthority::new(),
            viewer,
            stopped,
        })
    }

    #[must_use]
    pub fn media(&self) -> &MediaSession {
        &self.media
    }

    #[must_use]
    pub fn grants(&self) -> &GrantAuthority {
        &self.grants
    }

    #[must_use]
    pub fn viewer(&self) -> &ViewerBinding {
        &self.viewer
    }

    #[must_use]
    pub fn media_session_id(&self) -> &MediaSessionId {
        self.media.media_session_id()
    }

    #[must_use]
    pub fn state(&self) -> &MediaState {
        self.media.state()
    }

    /// Starts the offer only when the bound viewer already holds a live view
    /// grant for this media session.
    pub fn start(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        self.authorize_view_for_action(now_ms)?;
        let result = self.media.start(now_ms);
        self.revoke_if_expired(result)
    }

    /// Handles one inbound message from the authenticated sender.
    ///
    /// The sender must be the bound viewer. Answer/ICE (and every other
    /// non-terminal message) additionally require a live view grant for that
    /// exact device/Session; a revoked or unauthenticated peer cannot become
    /// the receiver or inject candidates.
    pub fn handle(
        &mut self,
        sender: &ViewerBinding,
        message: &SignallingMessage,
        now_ms: i64,
    ) -> Result<Option<SignallingMessage>, SignallingError> {
        if sender != &self.viewer {
            return Err(SignallingError::WrongPeer);
        }
        let ends_session = matches!(
            message,
            SignallingMessage::Stop { .. } | SignallingMessage::Failed { .. }
        );
        if !ends_session {
            self.authorize_view_for_action(now_ms)?;
        }
        let result = self.media.handle(message, now_ms);
        let stopped = ends_session && result.is_ok();
        if stopped || matches!(&result, Err(SignallingError::Expired)) {
            self.mark_stopped();
            self.grants.stop_media(self.media.media_session_id());
        }
        result
    }

    pub fn add_ice_candidate(
        &mut self,
        now_ms: i64,
        candidate: IceCandidate,
    ) -> Result<SignallingMessage, SignallingError> {
        self.authorize_view_for_action(now_ms)?;
        let result = self.media.add_ice_candidate(now_ms, candidate);
        self.revoke_if_expired(result)
    }

    pub fn ice_complete(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        self.authorize_view_for_action(now_ms)?;
        let result = self.media.ice_complete(now_ms);
        self.revoke_if_expired(result)
    }

    /// Pauses media and withdraws the controller lease so no input can be
    /// replayed across the gap.
    pub fn pause(
        &mut self,
        now_ms: i64,
        reason: PauseReason,
    ) -> Result<SignallingMessage, SignallingError> {
        let result = self.media.pause(now_ms, reason);
        let message = self.revoke_if_expired(result)?;
        if reason == PauseReason::ScreenLocked {
            self.grants.lock_media(self.media.media_session_id());
        } else {
            self.grants
                .release_controller_for_media(self.media.media_session_id());
        }
        Ok(message)
    }

    pub fn resume(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        let result = self.media.resume(now_ms);
        self.revoke_if_expired(result)
    }

    pub fn lock(&mut self, now_ms: i64) -> Result<SignallingMessage, SignallingError> {
        let result = self.media.lock(now_ms);
        let message = self.revoke_if_expired(result)?;
        self.grants.lock_media(self.media.media_session_id());
        Ok(message)
    }

    pub fn unlock(&mut self) -> Result<(), SignallingError> {
        self.media.unlock()?;
        self.grants.unlock_media(self.media.media_session_id());
        Ok(())
    }

    pub fn suspend_for_reconnect(
        &mut self,
        now_ms: i64,
    ) -> Result<SignallingMessage, SignallingError> {
        let result = self.media.suspend_for_reconnect(now_ms);
        let message = self.revoke_if_expired(result)?;
        self.grants
            .release_controller_for_media(self.media.media_session_id());
        Ok(message)
    }

    pub fn resume_after_reconnect(
        &mut self,
        now_ms: i64,
    ) -> Result<SignallingMessage, SignallingError> {
        let result = self.media.resume_after_reconnect(now_ms);
        self.revoke_if_expired(result)
    }

    pub fn stop(&mut self, reason: StopReason) -> Result<SignallingMessage, SignallingError> {
        let message = self.media.stop(reason)?;
        self.mark_stopped();
        self.grants.stop_media(self.media.media_session_id());
        Ok(message)
    }

    fn mark_stopped(&self) {
        self.stopped.mark_stopped(self.media.media_session_id());
    }

    /// Authorization for actions that send or accept signalling. Media expiry
    /// is terminal and revokes all authority; other view failures are typed.
    fn authorize_view_for_action(&mut self, now_ms: i64) -> Result<(), SignallingError> {
        match self.authorize_view(now_ms) {
            Ok(_) => Ok(()),
            Err(GrantError::SessionExpired) => {
                self.media.fail(FailureReason::Expired);
                self.mark_stopped();
                self.grants.stop_media(self.media.media_session_id());
                Err(SignallingError::Expired)
            }
            Err(error) => Err(SignallingError::ViewDenied(error)),
        }
    }

    fn ensure_media_grants_allowed(&self, now_ms: i64) -> Result<(), GrantError> {
        if self.stopped.is_stopped(self.media.media_session_id())
            || self.media.state().is_terminal()
        {
            return Err(GrantError::SessionClosed);
        }
        if self.media.is_expired_at(now_ms) {
            return Err(GrantError::SessionExpired);
        }
        Ok(())
    }

    fn revoke_if_expired(
        &mut self,
        result: Result<SignallingMessage, SignallingError>,
    ) -> Result<SignallingMessage, SignallingError> {
        if matches!(&result, Err(SignallingError::Expired)) {
            self.mark_stopped();
            self.grants.stop_media(self.media.media_session_id());
        }
        result
    }

    /// Issues a view grant for the bound viewer. The grant expiry is capped by
    /// the media session's absolute expiry.
    pub fn grant_view(&mut self, now_ms: i64, ttl_ms: i64) -> Result<ViewGrant, GrantError> {
        self.ensure_media_grants_allowed(now_ms)?;
        let media_expiry = self
            .media
            .expires_at_ms()
            .ok_or(GrantError::SessionClosed)?;
        let remaining = media_expiry.saturating_sub(now_ms);
        if remaining <= 0 {
            return Err(GrantError::SessionExpired);
        }
        self.grants.grant_view(
            self.viewer.device_id.clone(),
            self.viewer.session_id.clone(),
            self.media.media_session_id().clone(),
            now_ms,
            ttl_ms.min(remaining),
        )
    }

    pub fn grant_control(
        &mut self,
        view_grant_id: &GrantId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControlGrant, GrantError> {
        self.ensure_media_grants_allowed(now_ms)?;
        self.grants.grant_control(view_grant_id, now_ms, ttl_ms)
    }

    pub fn claim_controller(
        &mut self,
        control_grant_id: &GrantId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControllerLease, GrantError> {
        self.ensure_media_grants_allowed(now_ms)?;
        self.grants
            .claim_controller(control_grant_id, now_ms, ttl_ms)
    }

    pub fn renew_controller(
        &mut self,
        lease_id: &LeaseId,
        device_id: &DeviceId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControllerLease, GrantError> {
        self.ensure_media_grants_allowed(now_ms)?;
        self.grants
            .renew_controller(lease_id, device_id, now_ms, ttl_ms)
    }

    pub fn release_controller(
        &mut self,
        lease_id: &LeaseId,
        device_id: &DeviceId,
    ) -> Result<(), GrantError> {
        self.grants.release_controller(lease_id, device_id)
    }

    pub fn revoke_view(&mut self, grant_id: &GrantId) -> bool {
        self.grants.revoke_view(grant_id)
    }

    pub fn revoke_control(&mut self, grant_id: &GrantId) -> bool {
        self.grants.revoke_control(grant_id)
    }

    pub fn revoke_device(&mut self, device_id: &DeviceId) -> usize {
        self.grants.revoke_device(device_id)
    }

    /// Checks the bound viewer's live view grant, but only while the media
    /// session itself is neither expired nor terminal.
    pub fn authorize_view(&self, now_ms: i64) -> Result<&ViewGrant, GrantError> {
        self.ensure_media_grants_allowed(now_ms)?;
        self.grants.authorize_view(
            &self.viewer.device_id,
            &self.viewer.session_id,
            self.media.media_session_id(),
            now_ms,
        )
    }

    /// The single remote-input admission point.
    pub fn authorize_input(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        lease_id: &LeaseId,
        now_ms: i64,
    ) -> Result<(), InputDenial> {
        if device_id != &self.viewer.device_id || session_id != &self.viewer.session_id {
            return Err(InputDenial::WrongPeer);
        }
        if self.stopped.is_stopped(self.media.media_session_id()) {
            return Err(InputDenial::SessionStopped);
        }
        if self.grants.is_locked(self.media.media_session_id()) {
            return Err(InputDenial::Locked);
        }
        if self.media.is_expired_at(now_ms) {
            return Err(InputDenial::SessionExpired);
        }
        match self.media.state() {
            MediaState::Connected => {}
            MediaState::Paused(_) => return Err(InputDenial::SessionPaused),
            MediaState::Stopped(_) => return Err(InputDenial::SessionStopped),
            MediaState::Failed(_) => return Err(InputDenial::SessionFailed),
            MediaState::Idle | MediaState::OfferSent => {
                return Err(InputDenial::SessionNotConnected)
            }
        }
        self.grants
            .authorize_control_with_lease(
                device_id,
                session_id,
                self.media.media_session_id(),
                lease_id,
                now_ms,
            )
            .map(|_| ())
            .map_err(InputDenial::from)
    }
}
