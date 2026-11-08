//! Remote Screen media/signalling contract (M1).
//!
//! This crate is the **secure boundary** between the Host control plane and a
//! future media transport. It contains no OS capture backend, no WebRTC/ICE
//! implementation, no socket, no event log and no clock. It is deliberately
//! testable with two in-memory peers.
//!
//! ## Contract split
//!
//! * [`wire`] — versioned offer/answer/ICE/pause/resume/stop/failed messages,
//!   an absolute expiry, bounded SDP and candidate fields, and typed codec,
//!   transport and STUN/TURN descriptors. TURN credentials are redacted in
//!   `Debug`/`Display`.
//! * [`grant`] — separate [`ViewGrant`] and [`ControlGrant`] authority plus one
//!   absolute-expiry [`ControllerLease`] per media session. Control requires a
//!   live view grant, and input requires all three.
//! * [`session`] — the offer → answer → connected → paused → stopped/failed
//!   state machine, screen-lock pause and reconnect/resume handling.
//!
//! ## Adapter boundary
//!
//! A real capture adapter supplies an SDP [`Sdp`] and
//! [`MediaCapabilities`] to [`HostSession::new`], calls [`HostSession::start`],
//! and transports the returned [`SignallingMessage::encode`] bytes over an
//! authenticated control channel. A real WebRTC/ICE library is behind the
//! `Sdp`/`IceCandidate` values: this crate never parses a media frame, opens a
//! socket or contacts TURN. The iOS/desktop viewer does the mirror image with
//! [`MediaSession::viewer`] and hands [`MediaSession::handle`] output back to
//! its transport. Input adapters must call [`HostSession::authorize_input`]
//! before injecting one batch; they must not queue or replay a batch across a
//! pause, lock or reconnect.
//!
//! ## What is NOT VERIFIED
//!
//! **No real media path exists yet.** There is no WebRTC/DTLS-SRTP stack, no
//! real ICE gathering, no TURN/STUN connectivity test, no OS capture
//! (ScreenCaptureKit, Windows Graphics Capture, PipeWire/portal), no
//! VideoToolbox encoder, no iOS viewer, no Host RPC wiring and no physical
//! iPhone run. This crate proves the typed signalling, grant, lease and state
//! boundary with mock peers only; see [docs/REMOTE_SCREEN.md](../../../docs/REMOTE_SCREEN.md)
//! for the exact status table.

pub mod capability;
pub mod error;
pub mod grant;
pub mod limits;
pub mod session;
pub mod wire;

pub use capability::{
    validate_expiry, CodecDescriptor, CodecName, Credential, IceCandidate, IceServer, IceTransport,
    IceUrl, MediaCapabilities, MediaSessionId, Sdp, TransportCapability,
};
pub use error::{GrantError, InputDenial, SignallingError};
pub use grant::{ControlGrant, ControllerLease, GrantAuthority, GrantId, ViewGrant};
pub use limits::{
    MAX_ACTIVE_GRANTS, MAX_CAPABILITY_CODECS, MAX_CODEC_NAME_BYTES, MAX_CONTROLLER_LEASE_MS,
    MAX_CONTROL_GRANT_MS, MAX_CREDENTIAL_BYTES, MAX_ICE_CANDIDATE_BYTES, MAX_ICE_SERVERS,
    MAX_ICE_URLS_PER_SERVER, MAX_ICE_URL_BYTES, MAX_MEDIA_SESSION_MS, MAX_SDP_BYTES,
    MAX_SIGNALLING_MESSAGE_BYTES, MAX_VIEW_GRANT_MS, SIGNALLING_VERSION,
};
pub use session::{HostSession, MediaSession, MediaState, Role};
pub use wire::{FailureReason, PauseReason, SignallingMessage, StopReason};
