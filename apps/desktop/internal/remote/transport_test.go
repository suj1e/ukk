package remote

import (
	"context"
	"encoding/json"
	"errors"
	"sync"
	"testing"
	"time"
)

type testPipe struct {
	ch     chan []byte
	closed chan struct{}
	once   sync.Once
}

func newTestPipe(capacity int) *testPipe {
	if capacity <= 0 {
		capacity = 16
	}
	return &testPipe{ch: make(chan []byte, capacity), closed: make(chan struct{})}
}

func (p *testPipe) Send(raw []byte) error {
	cp := append([]byte(nil), raw...)
	select {
	case <-p.closed:
		return errors.New("test pipe closed")
	default:
	}
	select {
	case p.ch <- cp:
		return nil
	case <-p.closed:
		return errors.New("test pipe closed")
	}
}

func (p *testPipe) Receive(ctx context.Context) ([]byte, error) {
	select {
	case raw := <-p.ch:
		return raw, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-p.closed:
		return nil, errors.New("test pipe closed")
	}
}

func (p *testPipe) Close() error {
	p.once.Do(func() { close(p.closed) })
	return nil
}

type testEndpoint struct {
	out *testPipe
	in  *testPipe
}

func (e *testEndpoint) Send(raw []byte) error                       { return e.out.Send(raw) }
func (e *testEndpoint) Receive(ctx context.Context) ([]byte, error) { return e.in.Receive(ctx) }
func (e *testEndpoint) Close() error {
	_ = e.out.Close()
	return e.in.Close()
}

// transportPair returns two endpoints wired to each other.
func transportPair(capacity int) (client, host Transport) {
	toHost := newTestPipe(capacity)
	toClient := newTestPipe(capacity)
	return &testEndpoint{out: toHost, in: toClient}, &testEndpoint{out: toClient, in: toHost}
}

type tamperTransport struct {
	Transport
	tamper func([]byte) []byte
}

func (t *tamperTransport) Send(raw []byte) error { return t.Transport.Send(t.tamper(raw)) }

// tamperField flips the first byte of a base64url field inside a JSON object.
func tamperField(field string) func([]byte) []byte {
	return func(raw []byte) []byte {
		var object map[string]json.RawMessage
		if err := json.Unmarshal(raw, &object); err != nil {
			return raw
		}
		var text string
		if err := json.Unmarshal(object[field], &text); err != nil {
			return raw
		}
		value, err := decodeBase64URL(text)
		if err != nil || len(value) == 0 {
			return raw
		}
		value[0] ^= 0xff
		encoded, err := json.Marshal(encodeBase64URL(value))
		if err != nil {
			return raw
		}
		object[field] = encoded
		out, err := json.Marshal(object)
		if err != nil {
			return raw
		}
		return out
	}
}

type memTransport struct {
	mu       sync.Mutex
	incoming [][]byte
	sent     [][]byte
	notify   chan struct{}
	closed   chan struct{}
	once     sync.Once
}

func newMemTransport() *memTransport {
	return &memTransport{notify: make(chan struct{}, 1), closed: make(chan struct{})}
}

func (m *memTransport) push(raw []byte) {
	m.mu.Lock()
	m.incoming = append(m.incoming, append([]byte(nil), raw...))
	m.mu.Unlock()
	select {
	case m.notify <- struct{}{}:
	default:
	}
}

func (m *memTransport) Send(raw []byte) error {
	select {
	case <-m.closed:
		return errors.New("mem transport closed")
	default:
	}
	m.mu.Lock()
	m.sent = append(m.sent, append([]byte(nil), raw...))
	m.mu.Unlock()
	return nil
}

func (m *memTransport) Receive(ctx context.Context) ([]byte, error) {
	for {
		m.mu.Lock()
		if len(m.incoming) > 0 {
			raw := m.incoming[0]
			m.incoming = m.incoming[1:]
			m.mu.Unlock()
			return raw, nil
		}
		m.mu.Unlock()
		select {
		case <-m.notify:
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-m.closed:
			return nil, errors.New("mem transport closed")
		}
	}
}

func (m *memTransport) Close() error {
	m.once.Do(func() { close(m.closed) })
	return nil
}

func (m *memTransport) sentFrames() [][]byte {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make([][]byte, len(m.sent))
	copy(out, m.sent)
	return out
}

func testContext(t *testing.T) (context.Context, context.CancelFunc) {
	t.Helper()
	return context.WithTimeout(context.Background(), 5*time.Second)
}

type recordTransport struct {
	Transport
	mu   sync.Mutex
	sent [][]byte
}

func (r *recordTransport) Send(raw []byte) error {
	r.mu.Lock()
	r.sent = append(r.sent, append([]byte(nil), raw...))
	r.mu.Unlock()
	return r.Transport.Send(raw)
}

func (r *recordTransport) frames() [][]byte {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([][]byte, len(r.sent))
	copy(out, r.sent)
	return out
}
