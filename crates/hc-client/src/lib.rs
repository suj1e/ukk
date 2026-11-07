//! Typed client for herdr-core's protocol generation 1.
//!
//! Pinned to herdr-core v0.1.18 (`hc.openrpc.json`, Apache-2.0):
//! <https://github.com/herdrdev/herdr-core/tree/v0.1.18/schema>.
//!
//! Two surfaces exist:
//!
//! * [`HcClient`] keeps the small plain-mode one-shot request/watch helpers
//!   used by read-only Host probes. Calls are bounded and always reap the
//!   child process.
//! * [`FramedConnection`] negotiates `hello {framed:true}` and provides the
//!   typed control API plus framed terminal attach and event subscriptions.
//!
//! Capability checks come from the `hello` result. A method or feature the
//! connected core does not advertise returns [`HcError::Unsupported`]; this
//! crate never calls an undocumented method as a fallback.
//!
//! Lifecycle vocabulary is deliberately separate:
//!
//! * detach: send CLOSE for one stream, or simply close the client. Never
//!   touches the program.
//! * interrupt: `terminal.interrupt`, the documented stop key; never
//!   escalates.
//! * stop: `terminal.kill`, the documented program end. There is no softer
//!   stop method in protocol generation 1.
//! * archive: metadata in ukk; the runtime half is `terminal.rm`, which only
//!   accepts a finished record.

mod connection;
mod error;
mod events;
mod frame;
mod protocol;
mod stream;
mod types;

pub use connection::{
    FramedConnection, HcClient, LaunchOutcome, SendOutcome, DEFAULT_CALL_TIMEOUT,
    DEFAULT_REQUEST_TIMEOUT,
};
pub use error::{FrameError, HcError, RemoteError, RemoteKind, Result};
pub use events::{
    CursorStep, EventCursor, EventNotification, EventPayload, FinishedData, LabelsData, LostData,
    MissedNotification, StatusData, StreamNotice,
};
pub use frame::{Frame, FrameKind, FrameReader, FrameWriter, HEADER_LEN};
pub use stream::{
    Attachment, ChannelLifecycle, EventQueue, EventSubscription, StreamClose, StreamEvent,
    MAX_INPUT_CHUNK, MAX_STREAM_BUFFER_BYTES, MAX_STREAM_BUFFER_FRAMES,
};
pub use types::*;
