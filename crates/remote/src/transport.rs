//! Outbound relay connection seam.
//!
//! The pairing core is transport agnostic. This trait is the only seam a relay
//! adapter must implement: an ordered, reliable, **outbound** frame channel
//! that carries opaque bytes. The adapter owns TLS, relay authentication,
//! reconnect and its own wire JSON; this crate deliberately does not invent or
//! claim any alma-relay frame. See [`crate::relay`] for the versioned
//! ukk-internal signalling envelope that runs over this channel.
//!
//! ## Outbound WSS assumptions
//!
//! * The Host dials out to the relay. There is no inbound listener, port
//!   forwarding or TCP fallback; the peer never dials the Host.
//! * TLS is required and validated against the platform trust store; there is
//!   no insecure-skip-verify mode.
//! * The relay observes routing metadata and ciphertext only. Pairing
//!   handshake, challenge and grant payloads are Noise messages and are opaque
//!   to the relay.
//! * Frames are delivered in order on one connection. A relay reconnect is a
//!   new pairing attempt with a new invitation; started or completed invites
//!   are never replayed. [`RelayConnection::generation`] makes the reconnect
//!   boundary explicit so a driver can refuse messages attributed to an older
//!   connection.
//! * Frames are bounded by [`crate::MAX_PAIRING_FRAME_BYTES`] for pairing
//!   payloads and [`crate::relay::MAX_SIGNALLING_FRAME_BYTES`] for the
//!   signalling envelope. Adapters must reject larger frames before buffering
//!   them.
//! * The relay is untrusted. It cannot grant a device, change a scope, extend
//!   a grant, or authenticate a device on the Host's behalf.

use std::error::Error;

/// A reliable, ordered, outbound frame channel to an untrusted relay.
///
/// `send_frame` and `recv_frame` carry complete signalling envelopes; the
/// opaque pairing payload lives inside those envelopes. Implementations may be
/// synchronous wrappers around an async runtime; this crate performs no I/O and
/// its tests use an in-memory relay, never a socket.
///
/// `recv_frame(max_bytes)` is a hard allocation bound, not an advisory hint:
/// the adapter must not read or buffer more than `max_bytes` and must return
/// its own error if the peer declares or delivers a larger frame.
pub trait RelayConnection {
    type Error: Error + Send + Sync + 'static;

    /// Monotonic reconnect generation.
    ///
    /// `0` means "not connected yet". An adapter MUST advance the generation
    /// after every successful (re)connect and MUST never reuse or decrease it.
    /// The driver refuses to carry state across generations and rejects
    /// messages tagged with a stale generation.
    fn generation(&self) -> u64;

    /// Sends one complete signalling frame.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Self::Error>;

    /// Receives one complete signalling frame of at most `max_bytes` bytes.
    fn recv_frame(&mut self, max_bytes: usize) -> Result<Vec<u8>, Self::Error>;
}
