package relay

import (
	"errors"
	"strings"
	"testing"
)

func TestDecodeFrameStrict(t *testing.T) {
	valid := []byte(`{"t":"open","conn":"conn-1"}`)
	f, err := decodeFrame(valid)
	if err != nil || f.T != "open" || f.Conn != "conn-1" {
		t.Fatalf("valid frame = %+v, %v", f, err)
	}

	cases := map[string][]byte{
		"unknown-field": []byte(`{"t":"open","conn":"c","extra":1}`),
		"missing-type":  []byte(`{"conn":"c"}`),
		"trailing":      []byte(`{"t":"open"} {}`),
		"invalid-json":  []byte(`{"t":`),
		"empty":         nil,
	}
	for name, raw := range cases {
		t.Run(name, func(t *testing.T) {
			if _, ferr := decodeFrame(raw); ferr == nil || !ferr.Fatal() {
				t.Fatalf("error = %v, want fatal frame_invalid", ferr)
			} else if ferr.Code != CodeFrameInvalid {
				t.Fatalf("code = %s", ferr.Code)
			}
		})
	}

	encoded, encodeErr := encodeFrame(frame{T: "data", Conn: "c", Payload: "aGVsbG8="})
	if encodeErr != nil {
		t.Fatal(encodeErr)
	}
	decoded, ferr := decodeFrame(encoded)
	if ferr != nil || decoded.T != "data" || decoded.Payload != "aGVsbG8=" {
		t.Fatalf("round trip = %+v, %v", decoded, ferr)
	}
}

func TestValidID(t *testing.T) {
	for _, id := range []string{"a", "host-1", "device_1", "space:one", strings.Repeat("a", 128)} {
		if !validID(id) {
			t.Fatalf("validID(%q) = false", id)
		}
	}
	for _, id := range []string{"", "with space", "bad/slash", strings.Repeat("a", 129)} {
		if validID(id) {
			t.Fatalf("validID(%q) = true", id)
		}
	}
}

func TestPayloadBoundary(t *testing.T) {
	if err := validatePayload(strings.Repeat("A", maxEncodedPayload)); err != nil {
		t.Fatalf("boundary payload rejected: %v", err)
	}
	err := validatePayload(strings.Repeat("A", maxEncodedPayload+1))
	if err == nil || !err.Fatal() || err.Code != CodePayloadTooLarge {
		t.Fatalf("oversize payload error = %v", err)
	}
}

func TestCodeOf(t *testing.T) {
	err := newError(CodeUnknownConn, "missing")
	if !HasCode(err, CodeUnknownConn) || HasCode(err, CodeClosed) {
		t.Fatal("HasCode mismatch")
	}
	if CodeOf(errors.New("plain")) != "" {
		t.Fatal("plain error should have no code")
	}
}
