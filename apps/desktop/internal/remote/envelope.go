package remote

import (
	"crypto/cipher"
	"encoding/binary"
	"errors"

	"golang.org/x/crypto/chacha20poly1305"
)

const (
	// EnvelopeVersion is the only supported envelope version.
	EnvelopeVersion byte = 1
	// MaxPlaintext is the largest accepted envelope plaintext.
	MaxPlaintext = 1 << 20
	// envelopeHeaderSize is version(1) + stream(4) + counter(8).
	envelopeHeaderSize = 13
	// DefaultReplayWindow is the default number of counters kept per stream.
	DefaultReplayWindow = 1024
)

type envelopeCipher struct {
	aead cipher.AEAD
}

func newEnvelopeCipher(key Key) (envelopeCipher, error) {
	aead, err := chacha20poly1305.New(key[:])
	if err != nil {
		return envelopeCipher{}, wrapError(CodeEnvelopeInvalid, "cipher", err)
	}
	return envelopeCipher{aead: aead}, nil
}

func (c envelopeCipher) seal(stream uint32, counter uint64, plaintext []byte) ([]byte, error) {
	if len(plaintext) > MaxPlaintext {
		return nil, newError(CodeEnvelopeSize, "plaintext exceeds limit")
	}
	if counter == 0 {
		return nil, newError(CodeEnvelopeInvalid, "counter must start at 1")
	}
	var header [envelopeHeaderSize]byte
	header[0] = EnvelopeVersion
	binary.BigEndian.PutUint32(header[1:5], stream)
	binary.BigEndian.PutUint64(header[5:13], counter)
	var nonce [12]byte
	copy(nonce[0:8], header[5:13])
	copy(nonce[8:12], header[1:5])

	out := make([]byte, envelopeHeaderSize, envelopeHeaderSize+len(plaintext)+c.aead.Overhead())
	copy(out, header[:])
	return c.aead.Seal(out, nonce[:], plaintext, header[:]), nil
}

func (c envelopeCipher) open(raw []byte) (stream uint32, counter uint64, plaintext []byte, err error) {
	if len(raw) < envelopeHeaderSize+c.aead.Overhead() {
		return 0, 0, nil, newError(CodeEnvelopeInvalid, "envelope too short")
	}
	if raw[0] != EnvelopeVersion {
		return 0, 0, nil, newError(CodeEnvelopeVersion, "unsupported envelope version")
	}
	stream = binary.BigEndian.Uint32(raw[1:5])
	counter = binary.BigEndian.Uint64(raw[5:13])
	if counter == 0 {
		return 0, 0, nil, newError(CodeEnvelopeInvalid, "counter must start at 1")
	}
	if len(raw)-envelopeHeaderSize > MaxPlaintext+c.aead.Overhead() {
		return 0, 0, nil, newError(CodeEnvelopeSize, "ciphertext exceeds limit")
	}
	header := raw[:envelopeHeaderSize]
	var nonce [12]byte
	copy(nonce[0:8], header[5:13])
	copy(nonce[8:12], header[1:5])
	plaintext, err = c.aead.Open(nil, nonce[:], raw[envelopeHeaderSize:], header)
	if err != nil {
		return 0, 0, nil, wrapError(CodeEnvelopeAuth, "envelope authentication failed", err)
	}
	if len(plaintext) > MaxPlaintext {
		return 0, 0, nil, newError(CodeEnvelopeSize, "plaintext exceeds limit")
	}
	return stream, counter, plaintext, nil
}

// ReplayWindow accepts counters exactly once within a bounded window. It is
// not safe for concurrent use; Conn guards it with a mutex.
type ReplayWindow struct {
	size    int
	highest uint64
	seen    []bool
}

// NewReplayWindow creates a window. A non-positive size selects
// DefaultReplayWindow.
func NewReplayWindow(size int) *ReplayWindow {
	if size <= 0 {
		size = DefaultReplayWindow
	}
	return &ReplayWindow{size: size, seen: make([]bool, size)}
}

// Accept records counter and reports whether it is fresh. Duplicate counters,
// counters older than the window and counters beyond the window are rejected.
func (w *ReplayWindow) Accept(counter uint64) bool {
	if counter == 0 {
		return false
	}
	if w.highest == 0 {
		w.highest = counter
		w.seen[0] = true
		return true
	}
	if counter > w.highest {
		shift := counter - w.highest
		if shift >= uint64(w.size) {
			clear(w.seen)
		} else {
			copy(w.seen[shift:], w.seen[:w.size-int(shift)])
			clear(w.seen[:shift])
		}
		w.highest = counter
		w.seen[0] = true
		return true
	}
	delta := w.highest - counter
	if delta >= uint64(w.size) || w.seen[delta] {
		return false
	}
	w.seen[delta] = true
	return true
}

// Highest returns the highest accepted counter, or zero before any acceptance.
func (w *ReplayWindow) Highest() uint64 { return w.highest }

var errNoTransport = errors.New("remote: transport required")
