//! Relay boundary.
//!
//! The pairing core is transport agnostic. This trait is the only seam a relay
//! adapter must implement: an ordered, reliable, outbound WebSocket-style frame
//! channel that carries opaque bytes. The adapter owns TLS, reconnect, relay
//! authentication and its own wire JSON; this crate deliberately does not
//! invent any alma-relay frame.
//!
//! ## Outbound WSS assumptions
//!
//! * The Host dials out to the relay. There is no inbound listener, port
//!   forwarding or TCP fallback.
//! * TLS is required and validated against the platform trust store; there is
//!   no insecure-skip-verify mode.
//! * The relay observes routing metadata and ciphertext only. Pairing
//!   handshake, challenge and grant payloads are Noise messages and are opaque
//!   to the relay.
//! * Frames are delivered in order on one connection. A relay reconnect is a
//!   new pairing attempt with a new invitation; completed or started invites
//!   are never replayed.
//! * Frames are bounded by [`crate::error::MAX_PAIRING_FRAME_BYTES`]; adapters
//!   must reject larger frames before buffering them.
//! * The relay is untrusted. It cannot grant a device, change a scope, extend
//!   a grant, or authenticate a device on the Host's behalf.

use std::error::Error;

/// A reliable, ordered, outbound frame channel to an untrusted relay.
///
/// `send_frame` and `recv_frame` carry opaque pairing bytes. Implementations
/// may be synchronous wrappers around an async runtime; this crate performs no
/// I/O and tests never open a socket.
///
/// `recv_frame(max_bytes)` is a hard allocation bound, not an advisory hint:
/// the adapter must not read or buffer more than `max_bytes` and must return
/// its own error if the peer declares or delivers a larger frame. The pairing
/// core passes [`crate::error::MAX_PAIRING_FRAME_BYTES`].
pub trait RelayTransport {
    type Error: Error + Send + Sync + 'static;

    /// Sends one complete pairing frame.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error>;

    /// Receives one complete pairing frame of at most `max_bytes` bytes.
    fn recv_frame(&mut self, max_bytes: usize) -> Result<Vec<u8>, Self::Error>;
}
