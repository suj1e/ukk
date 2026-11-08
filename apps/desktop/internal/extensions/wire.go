package extensions

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
)

// ProtocolVersion is the helper stdio protocol version.
const ProtocolVersion = 1

// Helper methods.
const (
	MethodHello    = "plugin.hello"
	MethodHealth   = "plugin.health"
	MethodInvoke   = "command.invoke"
	MethodShutdown = "plugin.shutdown"
)

// Request is a host-to-helper frame.
type Request struct {
	ID       uint64          `json:"id"`
	Method   string          `json:"method"`
	PluginID string          `json:"plugin_id,omitempty"`
	Protocol int             `json:"protocol,omitempty"`
	Command  string          `json:"command,omitempty"`
	Session  string          `json:"session_id,omitempty"`
	Args     json.RawMessage `json:"args,omitempty"`
}

// Response is a helper-to-host frame. A response always echoes the request id;
// event delivery in v1 is host-initiated only, so an unsolicited response is a
// protocol violation.
type Response struct {
	ID     uint64          `json:"id"`
	OK     bool            `json:"ok"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *WireError      `json:"error,omitempty"`
}

// WireError is a helper-authored error. Its code and message are untrusted
// text and are redacted before they reach an audit record.
type WireError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

// HelloResult is the helper's identity claim and contribution subset. The
// supervisor rejects any id the manifest did not declare.
type HelloResult struct {
	PluginID string   `json:"plugin_id"`
	Protocol int      `json:"protocol"`
	Commands []string `json:"commands,omitempty"`
	Views    []string `json:"views,omitempty"`
	Events   []string `json:"events,omitempty"`
}

// WriteFrame writes one length-prefixed (u32 big-endian) JSON frame. It refuses
// to emit a frame larger than MaxFrameBytes.
func WriteFrame(w io.Writer, value any) error {
	payload, err := json.Marshal(value)
	if err != nil {
		return errf(CodeInvalidArgs, "encode frame: %s", err.Error())
	}
	if len(payload) > MaxFrameBytes {
		return errf(CodeFrameTooLarge, "frame of %d bytes exceeds %d", len(payload), MaxFrameBytes)
	}
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], uint32(len(payload)))
	if err := writeAll(w, header[:]); err != nil {
		return errf(CodeHelperFailed, "write frame header: %s", err.Error())
	}
	if err := writeAll(w, payload); err != nil {
		return errf(CodeHelperFailed, "write frame body: %s", err.Error())
	}
	return nil
}

func writeAll(w io.Writer, data []byte) error {
	for len(data) > 0 {
		n, err := w.Write(data)
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
		data = data[n:]
	}
	return nil
}

// ReadFrame reads one length-prefixed JSON frame. It rejects empty frames and
// any declared length above MaxFrameBytes before allocating.
func ReadFrame(r io.Reader) ([]byte, error) {
	var header [4]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return nil, err
	}
	length := binary.BigEndian.Uint32(header[:])
	if length == 0 {
		return nil, errf(CodeProtocolViolation, "empty frame")
	}
	if length > MaxFrameBytes {
		return nil, errf(CodeFrameTooLarge, "frame of %d bytes exceeds %d", length, MaxFrameBytes)
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(r, payload); err != nil {
		return nil, errf(CodeProtocolViolation, "truncated frame: %s", err.Error())
	}
	return payload, nil
}

// decodeStrict decodes one JSON value with unknown fields rejected and no
// trailing data.
func decodeStrict(data []byte, value any) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if err := dec.Decode(value); err != nil {
		return err
	}
	var extra json.RawMessage
	if err := dec.Decode(&extra); err != io.EOF {
		return fmt.Errorf("trailing data after JSON value")
	}
	return nil
}
