package hc

import (
	"bytes"
	"encoding/binary"
	"errors"
	"testing"
)

func TestFrameRoundTrip(t *testing.T) {
	payload := []byte("hello\x00world")
	encoded, err := EncodeFrame(7, KindAutomaticInput, payload)
	if err != nil {
		t.Fatalf("EncodeFrame: %v", err)
	}
	if len(encoded) != FrameHeaderSize+len(payload) {
		t.Fatalf("encoded length = %d", len(encoded))
	}
	frame, err := ReadFrame(bytes.NewReader(encoded))
	if err != nil {
		t.Fatalf("ReadFrame: %v", err)
	}
	if frame.Channel != 7 || frame.Kind != KindAutomaticInput || !bytes.Equal(frame.Payload, payload) {
		t.Fatalf("round trip = %+v", frame)
	}

	empty, err := EncodeFrame(0, KindClose, nil)
	if err != nil {
		t.Fatalf("EncodeFrame empty: %v", err)
	}
	frame, err = ReadFrame(bytes.NewReader(empty))
	if err != nil || frame.Channel != 0 || frame.Kind != KindClose || len(frame.Payload) != 0 {
		t.Fatalf("empty frame = %+v, %v", frame, err)
	}
}

func TestFrameRejectsOversizedPayload(t *testing.T) {
	if _, err := EncodeFrame(1, KindData, make([]byte, MaxPayload+1)); !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("EncodeFrame oversized err = %v, want ErrFrameTooLarge", err)
	}

	var header [FrameHeaderSize]byte
	binary.LittleEndian.PutUint32(header[0:4], 1)
	header[4] = KindData
	binary.LittleEndian.PutUint32(header[5:9], uint32(MaxPayload+1))
	if _, err := ReadFrame(bytes.NewReader(header[:])); !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("ReadFrame oversized err = %v, want ErrFrameTooLarge (no payload supplied)", err)
	}
}

func TestFrameTruncatedIsProtocolError(t *testing.T) {
	encoded, err := EncodeFrame(2, KindData, []byte("abcdef"))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ReadFrame(bytes.NewReader(encoded[:len(encoded)-2])); !errors.Is(err, ErrProtocol) {
		t.Fatalf("truncated payload err = %v, want ErrProtocol", err)
	}
	if _, err := ReadFrame(bytes.NewReader(encoded[:4])); !errors.Is(err, ErrProtocol) {
		t.Fatalf("truncated header err = %v, want ErrProtocol", err)
	}
}
