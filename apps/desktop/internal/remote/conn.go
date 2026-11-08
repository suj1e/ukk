package remote

import (
	"context"
	"sync"
	"sync/atomic"
)

// Transport is an ordered, reliable frame transport. Send and Receive carry
// exactly one envelope (or handshake message) per call. Send must be safe for
// concurrent use; Close unblocks Receive.
type Transport interface {
	Send([]byte) error
	Receive(context.Context) ([]byte, error)
	Close() error
}

// Role selects envelope key direction.
type Role int

const (
	// RoleClient sends with the c2s key and receives with s2c.
	RoleClient Role = iota
	// RoleHost sends with the s2c key and receives with c2s.
	RoleHost
)

// MaxStreams bounds concurrently used stream identifiers in one direction.
const MaxStreams = 256

// Conn is an encrypted session over a Transport. Sends are serialized and
// ordered; each stream has its own monotonic counter.
type Conn struct {
	transport Transport
	sendCiph  envelopeCipher
	recvCiph  envelopeCipher

	sendMu       sync.Mutex
	sendCounters map[uint32]uint64

	recvMu      sync.Mutex
	recvWindows map[uint32]*ReplayWindow

	closed atomic.Bool
}

// NewConn binds a Transport to a completed handshake Session.
func NewConn(t Transport, session *Session, role Role) (*Conn, error) {
	if t == nil {
		return nil, wrapError(CodeTransport, "transport required", errNoTransport)
	}
	if session == nil {
		return nil, newError(CodeEnvelopeInvalid, "session required")
	}
	var sendKey, recvKey Key
	switch role {
	case RoleClient:
		sendKey, recvKey = session.C2S, session.S2C
	case RoleHost:
		sendKey, recvKey = session.S2C, session.C2S
	default:
		return nil, newError(CodeEnvelopeInvalid, "unknown role")
	}
	sendCiph, err := newEnvelopeCipher(sendKey)
	if err != nil {
		return nil, err
	}
	recvCiph, err := newEnvelopeCipher(recvKey)
	if err != nil {
		return nil, err
	}
	return &Conn{
		transport:    t,
		sendCiph:     sendCiph,
		recvCiph:     recvCiph,
		sendCounters: make(map[uint32]uint64),
		recvWindows:  make(map[uint32]*ReplayWindow),
	}, nil
}

// SendEnvelope encrypts and sends plaintext on stream. Concurrent calls are
// safe and their envelopes reach the transport in call order.
func (c *Conn) SendEnvelope(stream uint32, plaintext []byte) error {
	if c == nil {
		return newError(CodeClosed, "connection required")
	}
	c.sendMu.Lock()
	defer c.sendMu.Unlock()
	if c.closed.Load() {
		return newError(CodeClosed, "connection closed")
	}
	if len(plaintext) > MaxPlaintext {
		return newError(CodeEnvelopeSize, "plaintext exceeds limit")
	}
	counter, exists := c.sendCounters[stream]
	if !exists && len(c.sendCounters) >= MaxStreams {
		return newError(CodeEnvelopeStream, "stream limit reached")
	}
	counter++
	// Consume the counter before the transport call so a failed send can never
	// cause a nonce to be reused by a later attempt.
	c.sendCounters[stream] = counter
	envelope, err := c.sendCiph.seal(stream, counter, plaintext)
	if err != nil {
		return err
	}
	if err := c.transport.Send(envelope); err != nil {
		return wrapError(CodeTransport, "send envelope", err)
	}
	return nil
}

// ReceiveEnvelope receives and authenticates one envelope, enforcing the
// per-stream replay window.
func (c *Conn) ReceiveEnvelope(ctx context.Context) (uint32, []byte, error) {
	if c == nil {
		return 0, nil, newError(CodeClosed, "connection required")
	}
	c.recvMu.Lock()
	defer c.recvMu.Unlock()
	if c.closed.Load() {
		return 0, nil, newError(CodeClosed, "connection closed")
	}
	raw, err := c.transport.Receive(ctx)
	if err != nil {
		return 0, nil, wrapError(CodeTransport, "receive envelope", err)
	}
	stream, counter, plaintext, err := c.recvCiph.open(raw)
	if err != nil {
		return 0, nil, err
	}
	window, exists := c.recvWindows[stream]
	if !exists {
		if len(c.recvWindows) >= MaxStreams {
			return 0, nil, newError(CodeEnvelopeStream, "stream limit reached")
		}
		window = NewReplayWindow(DefaultReplayWindow)
		c.recvWindows[stream] = window
	}
	if !window.Accept(counter) {
		return 0, nil, newError(CodeEnvelopeReplay, "duplicate or stale counter")
	}
	return stream, plaintext, nil
}

// Close closes the underlying transport. It is safe to call more than once.
func (c *Conn) Close() error {
	if c == nil {
		return nil
	}
	if c.closed.Swap(true) {
		return nil
	}
	return c.transport.Close()
}

// Closed reports whether Close has been called.
func (c *Conn) Closed() bool { return c != nil && c.closed.Load() }
