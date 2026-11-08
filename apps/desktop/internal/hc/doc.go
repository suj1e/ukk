// Package hc is the desktop's typed client for herdr-core.
//
// It speaks protocol generation 1 to a child process started as
// `hc connect`, over that child's stdin and stdout. The package is pinned to
// herdr-core v0.1.18 (commit 3965a46caae095a3ead01de237fa13955e52b8c6); the
// method names, parameter names and result fields mirror `hc schema` and the
// upstream protocol README for that release instead of guessing.
//
// # Connection modes
//
// A connection starts in plain mode: JSON-RPC 2.0, one message per line. The
// client sends hello with framed=true and generation=1, and only switches to
// framed mode after a successful reply that confirms both. Every later
// message is a frame:
//
//	[channel u32 LE][kind u8][len u32 LE][payload]
//
// Channel 0 carries JSON-RPC. Non-zero channels carry terminal streams
// (terminal.attach) or event notification lines (events.subscribe). Payloads
// larger than 16 MiB are rejected from the header before any allocation and
// the connection is closed; unknown frame kinds are skipped.
//
// # Terminal streams
//
// An Attachment carries the raw terminal bytes from terminal.attach. Reads
// block until bytes, a core CLOSE, or Close; every byte handed to the caller
// is acknowledged with a CREDIT frame for exactly that many bytes. Human input
// is DATA, terminal-generated replies are AUTOMATIC_INPUT (only when both the
// core and the attach reply advertise it), and Resize/Focus use their own
// frame kinds. Observe attachments refuse writes, automatic input, resizes and
// focus changes locally: an observer never types and never owns the size.
//
// # Events
//
// SubscribeEvents returns the snapshot and a typed stream of fact changes.
// Notifications on the subscription channel are newline-delimited JSON. The
// client detects sequence gaps only for unfiltered subscriptions, replaces the
// snapshot on `missed`, and never invents sequence numbers on attach streams.
//
// # Process lifetime
//
// The child is owned by this package: it is killed and reaped on handshake
// failure, protocol violation, connection loss, context cancellation and
// Close. No terminal or Agent is stopped by closing an attachment, detaching a
// view, or closing this client; those are core-owned processes.
package hc
