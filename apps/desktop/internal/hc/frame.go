package hc

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

// Frame-level constants from protocol generation 1.
const (
	// FrameHeaderSize is [channel u32 LE][kind u8][len u32 LE].
	FrameHeaderSize = 9
	// MaxPayload is the absolute payload ceiling. The core uses the same
	// value; a larger length is rejected before a buffer is allocated.
	MaxPayload = 16 << 20

	KindData           uint8 = 0
	KindCredit         uint8 = 1
	KindClose          uint8 = 2
	KindResize         uint8 = 3
	KindFocus          uint8 = 4
	KindFacts          uint8 = 5
	KindAutomaticInput uint8 = 6
)

// ErrFrameTooLarge reports a frame whose declared payload exceeds MaxPayload.
// It is returned from the header alone, without allocating the payload.
var ErrFrameTooLarge = errors.New("hc: frame payload exceeds 16 MiB")

// Frame is one decoded framed-mode message.
type Frame struct {
	Channel uint32
	Kind    uint8
	Payload []byte
}

// EncodeFrame encodes one frame. It rejects an oversized payload before
// allocating the output buffer.
func EncodeFrame(channel uint32, kind uint8, payload []byte) ([]byte, error) {
	if len(payload) > MaxPayload {
		return nil, ErrFrameTooLarge
	}
	out := make([]byte, FrameHeaderSize+len(payload))
	binary.LittleEndian.PutUint32(out[0:4], channel)
	out[4] = kind
	binary.LittleEndian.PutUint32(out[5:9], uint32(len(payload)))
	copy(out[FrameHeaderSize:], payload)
	return out, nil
}

// ReadFrame reads one frame. It validates the declared length from the header
// before allocating the payload. A clean EOF between frames returns io.EOF; a
// truncated header or payload is a protocol error.
func ReadFrame(r io.Reader) (Frame, error) {
	var header [FrameHeaderSize]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		if errors.Is(err, io.EOF) {
			return Frame{}, io.EOF
		}
		if errors.Is(err, io.ErrUnexpectedEOF) {
			return Frame{}, fmt.Errorf("hc: truncated frame header: %w", ErrProtocol)
		}
		return Frame{}, err
	}
	channel := binary.LittleEndian.Uint32(header[0:4])
	kind := header[4]
	length := binary.LittleEndian.Uint32(header[5:9])
	if uint64(length) > MaxPayload {
		return Frame{}, ErrFrameTooLarge
	}
	payload := make([]byte, int(length))
	if _, err := io.ReadFull(r, payload); err != nil {
		if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
			return Frame{}, fmt.Errorf("hc: truncated frame payload: %w", ErrProtocol)
		}
		return Frame{}, err
	}
	return Frame{Channel: channel, Kind: kind, Payload: payload}, nil
}
