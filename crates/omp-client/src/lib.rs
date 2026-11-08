//! Typed client for `omp --mode rpc`.
//!
//! This crate owns **one** long-lived omp child process: it spawns it with
//! separate stdin/stdout/stderr pipes, waits for the `ready` handshake,
//! correlates command responses by `id`, and turns stdout JSON lines into
//! typed events. It never parses terminal text and never infers tool
//! execution or approvals.
//!
//! Protocol v1 is the only negotiated transport. Oversized inbound lines and
//! unexpected `rpc_chunk` frames are reported as typed transport events
//! instead of being guessed at.
//!
//! The Host owns the client. UI panes may open and close freely; dropping a
//! view must never call [`OmpClient::shutdown`]. Only the Host process (or an
//! explicit user "stop" action routed through the Host) terminates the child.
//!
//! See `docs/OMP_RPC.md` for the transport contract and for the semantics
//! that are currently NOT VERIFIED.

mod client;
mod config;
mod error;
mod event;
mod io;
mod redact;
mod snapshot;
mod wire;

pub use client::{Ack, EventReceiver, OmpClient, OmpState, PromptAck};
pub use config::OmpConfig;
pub use error::OmpError;
pub use event::{Event, EventKind, MessageEvent, ToolEvent};
pub use redact::redact;
pub use snapshot::{
    OmpExit, OmpSnapshot, OmpTimelineEntry, OmpTimelineKind, OmpTurn, OmpTurnState, OMP_PROVENANCE,
    OMP_RPC_PROTOCOL_VERSION,
};
pub use wire::{ReadyInfo, ResponseFrame, PROTOCOL_VERSION};
