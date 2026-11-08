//! Typed failures for signalling, grants and input admission.
//!
//! No variant carries SDP, an ICE candidate, a TURN credential or a media
//! frame. Callers can log the error directly without leaking transport
//! secrets.

use std::error::Error;
use std::fmt;

/// Failure parsing, validating or advancing a signalling session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignallingError {
    /// The envelope `v` field is not [`crate::SIGNALLING_VERSION`].
    UnsupportedVersion { found: u8 },
    /// Encoded bytes exceed the signalling envelope bound.
    Oversized { actual: usize, max: usize },
    /// Not a single JSON object, unknown fields, wrong types or trailing input.
    Malformed,
    /// A typed field violates its contract.
    Invalid(&'static str),
    /// The message is valid but not valid for this role.
    WrongRole(&'static str),
    /// The message is valid but not valid in the current state.
    UnexpectedMessage(&'static str),
    /// The media session has already stopped or failed.
    Terminal,
    /// Offer/answer names another media session.
    WrongSession,
    /// Answer expiry differs from the offer expiry.
    ExpiryMismatch,
    /// The absolute expiry has passed.
    Expired,
    /// The operation requires `connected`.
    NotConnected,
    /// No codec common to both capability sets.
    NoCommonCodec,
    /// No transport common to both capability sets.
    NoCommonTransport,
    /// The answer was not a subset of the offer.
    InvalidAnswer(&'static str),
    /// The screen is locked; the session is paused until explicit unlock/resume.
    Locked,
}

impl fmt::Display for SignallingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found } => {
                write!(
                    formatter,
                    "media signalling version {found} is not supported"
                )
            }
            Self::Oversized { actual, max } => {
                write!(
                    formatter,
                    "signalling message is {actual} bytes, limit is {max}"
                )
            }
            Self::Malformed => formatter.write_str("media signalling message is malformed"),
            Self::Invalid(reason) => write!(formatter, "invalid media signalling field: {reason}"),
            Self::WrongRole(reason) => write!(formatter, "wrong media peer role: {reason}"),
            Self::UnexpectedMessage(state) => {
                write!(formatter, "unexpected signalling message in state {state}")
            }
            Self::Terminal => formatter.write_str("media session is already stopped or failed"),
            Self::WrongSession => {
                formatter.write_str("signalling message names another media session")
            }
            Self::ExpiryMismatch => {
                formatter.write_str("answer expiry does not match the offer expiry")
            }
            Self::Expired => formatter.write_str("media session has expired"),
            Self::NotConnected => formatter.write_str("media session is not connected"),
            Self::NoCommonCodec => formatter.write_str("no common media codec"),
            Self::NoCommonTransport => formatter.write_str("no common media transport"),
            Self::InvalidAnswer(reason) => write!(formatter, "invalid media answer: {reason}"),
            Self::Locked => formatter.write_str("screen is locked; control is paused"),
        }
    }
}

impl Error for SignallingError {}

/// Failure issuing or checking a view/control grant or controller lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    InvalidTtl,
    Capacity,
    AlreadyGranted,
    ViewGrantMissing,
    ViewGrantExpired,
    ViewGrantMismatch,
    ControlGrantMissing,
    ControlGrantExpired,
    ControlGrantMismatch,
    ControlRequiresView,
    LeaseMissing,
    LeaseExpired,
    LeaseMismatch,
    ControllerAlreadyBound,
    SessionClosed,
    Locked,
    DeviceMismatch,
    MediaSessionMismatch,
}

impl fmt::Display for GrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTtl => formatter.write_str("grant lifetime is out of range"),
            Self::Capacity => formatter.write_str("too many active media grants"),
            Self::AlreadyGranted => formatter.write_str("an equivalent live grant already exists"),
            Self::ViewGrantMissing => formatter.write_str("view grant is missing"),
            Self::ViewGrantExpired => formatter.write_str("view grant has expired"),
            Self::ViewGrantMismatch => {
                formatter.write_str("view grant does not match this session")
            }
            Self::ControlGrantMissing => formatter.write_str("control grant is missing"),
            Self::ControlGrantExpired => formatter.write_str("control grant has expired"),
            Self::ControlGrantMismatch => {
                formatter.write_str("control grant does not match this session")
            }
            Self::ControlRequiresView => {
                formatter.write_str("control requires a live view grant for the same session")
            }
            Self::LeaseMissing => formatter.write_str("controller lease is missing"),
            Self::LeaseExpired => formatter.write_str("controller lease has expired"),
            Self::LeaseMismatch => formatter.write_str("controller lease does not match this peer"),
            Self::ControllerAlreadyBound => {
                formatter.write_str("another controller is already bound to this media session")
            }
            Self::SessionClosed => formatter.write_str("media session is stopped"),
            Self::Locked => formatter.write_str("screen is locked"),
            Self::DeviceMismatch => formatter.write_str("device does not own this grant"),
            Self::MediaSessionMismatch => formatter.write_str("grant names another media session"),
        }
    }
}

impl Error for GrantError {}

/// Failure admitting one remote input batch.
///
/// This combines media-session state with the grant/lease checks so a transport
/// has one typed decision point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDenial {
    WrongRole,
    SessionNotConnected,
    SessionPaused,
    SessionStopped,
    SessionFailed,
    SessionExpired,
    Locked,
    MissingViewGrant,
    ViewGrantExpired,
    MissingControlGrant,
    ControlGrantExpired,
    ControllerLeaseMissing,
    ControllerLeaseExpired,
    ControllerLeaseMismatch,
    Denied,
}

impl fmt::Display for InputDenial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::WrongRole => "only the capture-side peer may admit remote input",
            Self::SessionNotConnected => "media session is not connected",
            Self::SessionPaused => "media session is paused",
            Self::SessionStopped => "media session is stopped",
            Self::SessionFailed => "media session has failed",
            Self::SessionExpired => "media session has expired",
            Self::Locked => "screen is locked",
            Self::MissingViewGrant => "view grant is missing",
            Self::ViewGrantExpired => "view grant has expired",
            Self::MissingControlGrant => "control grant is missing",
            Self::ControlGrantExpired => "control grant has expired",
            Self::ControllerLeaseMissing => "controller lease is missing",
            Self::ControllerLeaseExpired => "controller lease has expired",
            Self::ControllerLeaseMismatch => "controller lease does not match this peer",
            Self::Denied => "remote input was denied",
        };
        formatter.write_str(text)
    }
}

impl Error for InputDenial {}

impl From<GrantError> for InputDenial {
    fn from(error: GrantError) -> Self {
        match error {
            GrantError::ViewGrantMissing | GrantError::ControlRequiresView => {
                Self::MissingViewGrant
            }
            GrantError::ViewGrantExpired => Self::ViewGrantExpired,
            GrantError::ControlGrantMissing => Self::MissingControlGrant,
            GrantError::ControlGrantExpired => Self::ControlGrantExpired,
            GrantError::LeaseMissing => Self::ControllerLeaseMissing,
            GrantError::LeaseExpired => Self::ControllerLeaseExpired,
            GrantError::LeaseMismatch
            | GrantError::DeviceMismatch
            | GrantError::MediaSessionMismatch
            | GrantError::ControlGrantMismatch
            | GrantError::ViewGrantMismatch => Self::ControllerLeaseMismatch,
            GrantError::Locked => Self::Locked,
            GrantError::SessionClosed => Self::SessionStopped,
            GrantError::InvalidTtl
            | GrantError::Capacity
            | GrantError::AlreadyGranted
            | GrantError::ControllerAlreadyBound => Self::Denied,
        }
    }
}
