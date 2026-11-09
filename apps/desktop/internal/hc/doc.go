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
//
// # Documents
//
// DocGet and DocSet read and write a named core document with a version
// compare-and-swap. The value is passed through as raw JSON so callers own the
// schema. A stale version fails with the core error kind conflict; read again
// before retrying. Values larger than MaxDocValueBytes are rejected locally
// before they are marshaled or sent.
package hc

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
)

const (
	// MaxDocValueBytes bounds one doc.set value. The world document is much
	// smaller in practice; the limit keeps a broken caller from allocating or
	// sending an unbounded request.
	MaxDocValueBytes = 4 << 20

	// MaxDocNameLength bounds the document name. Core document names are short
	// identifiers such as "herdr-app".
	MaxDocNameLength = 256

	// maxDocRequestJSON lets a document request carry the full value plus the
	// small JSON-RPC envelope. Ordinary parameters stay at maxRequestJSON.
	maxDocRequestJSON = MaxDocValueBytes + 64<<10
)

// DocGet reads the document named name. on names the machine whose document
// is read; an empty on asks the core for its local document. A missing
// document is reported as a nil value, not an error.
func (c *Client) DocGet(ctx context.Context, name, on string) (json.RawMessage, uint64, error) {
	if err := checkDocName("doc.get", name); err != nil {
		return nil, 0, err
	}
	params := struct {
		Name string `json:"name"`
		On   string `json:"on,omitempty"`
	}{Name: name, On: on}
	var out struct {
		Value   json.RawMessage `json:"value"`
		Version *uint64         `json:"version"`
	}
	if err := c.call(ctx, "doc.get", params, &out, nil); err != nil {
		return nil, 0, err
	}
	if out.Version == nil {
		return nil, 0, &Error{Code: CodeProtocol, Method: "doc.get", Message: "missing version"}
	}
	value := out.Value
	if isNullJSON(value) {
		value = nil
	}
	return value, *out.Version, nil
}

// DocSet replaces the document named name when version matches the version
// last read, and returns the new version. on has the same meaning as in
// DocGet. A nil or empty value, invalid JSON, or a value larger than
// MaxDocValueBytes is rejected without sending a request.
func (c *Client) DocSet(ctx context.Context, name, on string, version uint64, value json.RawMessage) (uint64, error) {
	if err := checkDocName("doc.set", name); err != nil {
		return 0, err
	}
	if len(value) == 0 {
		return 0, &Error{Code: CodeProtocol, Method: "doc.set", Message: "value required"}
	}
	if len(value) > MaxDocValueBytes {
		return 0, &Error{
			Code:    CodeRequestTooLarge,
			Method:  "doc.set",
			Message: fmt.Sprintf("value exceeds %d bytes", MaxDocValueBytes),
		}
	}
	if !json.Valid(value) {
		return 0, &Error{Code: CodeProtocol, Method: "doc.set", Message: "value is not valid JSON"}
	}
	params := struct {
		Name    string          `json:"name"`
		On      string          `json:"on,omitempty"`
		Version uint64          `json:"version"`
		Value   json.RawMessage `json:"value"`
	}{Name: name, On: on, Version: version, Value: value}
	var out struct {
		Version *uint64 `json:"version"`
	}
	if err := c.callWithLimit(ctx, "doc.set", params, &out, nil, maxDocRequestJSON); err != nil {
		return 0, err
	}
	if out.Version == nil {
		return 0, &Error{Code: CodeProtocol, Method: "doc.set", Message: "missing version"}
	}
	return *out.Version, nil
}

func checkDocName(method, name string) error {
	if name == "" {
		return &Error{Code: CodeProtocol, Method: method, Message: "name required"}
	}
	if len(name) > MaxDocNameLength {
		return &Error{
			Code:    CodeProtocol,
			Method:  method,
			Message: fmt.Sprintf("name exceeds %d bytes", MaxDocNameLength),
		}
	}
	return nil
}

func isNullJSON(raw json.RawMessage) bool {
	return len(raw) == 0 || bytes.Equal(bytes.TrimSpace(raw), []byte("null"))
}
