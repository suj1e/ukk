package relay

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
)

// Wire limits. They are exported so clients and tests share the same bounds.
const (
	MaxReadBytes      = 16 << 20
	MaxPayloadBytes   = 1 << 20
	QueueFrames       = 1024
	MaxConnsPerDevice = 8
	MaxDevicesPerHost = 32
)

// maxEncodedPayload bounds the base64 text before any decode. The relay never
// decodes payloads; this length check is sufficient because valid base64 of a
// longer input cannot fit.
var maxEncodedPayload = base64.StdEncoding.EncodedLen(MaxPayloadBytes)

// frame is the relay control envelope. Payload is opaque base64 text and is
// forwarded unchanged.
type frame struct {
	T        string `json:"t"`
	HostID   string `json:"host_id,omitempty"`
	DeviceID string `json:"device_id,omitempty"`
	Conn     string `json:"conn,omitempty"`
	Payload  string `json:"payload,omitempty"`
	Device   string `json:"device,omitempty"`
	Online   *bool  `json:"online,omitempty"`
	Code     string `json:"code,omitempty"`
	Message  string `json:"message,omitempty"`
}

func encodeFrame(f frame) ([]byte, error) {
	raw, err := json.Marshal(f)
	if err != nil {
		return nil, err
	}
	if len(raw) > MaxReadBytes {
		return nil, errors.New("relay: encoded frame exceeds limit")
	}
	return raw, nil
}

func decodeFrame(raw []byte) (frame, *Error) {
	if len(raw) == 0 || len(raw) > MaxReadBytes {
		return frame{}, newFatal(CodeFrameInvalid, "frame size out of range")
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	var f frame
	if err := dec.Decode(&f); err != nil {
		return frame{}, newFatal(CodeFrameInvalid, "frame is not valid JSON")
	}
	if err := dec.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return frame{}, newFatal(CodeFrameInvalid, "trailing frame data")
	}
	if f.T == "" {
		return frame{}, newFatal(CodeFrameInvalid, "frame type required")
	}
	return f, nil
}

func validID(s string) bool {
	if len(s) == 0 || len(s) > 128 {
		return false
	}
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9':
		case c == '-', c == '_', c == '.', c == ':':
		default:
			return false
		}
	}
	return true
}

func validatePayload(payload string) *Error {
	if len(payload) > maxEncodedPayload {
		return newFatal(CodePayloadTooLarge, "payload exceeds 1 MiB")
	}
	return nil
}
