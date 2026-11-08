package bridge

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/coder/websocket"
)

// relayHost keeps one outbound authenticated connection to the relay and
// exposes each routed device connection as an ordered byte transport.
type relayHost struct {
	url    string
	token  string
	hostID string
	log    *slog.Logger
	onConn func(connID string, transport *relayConn)

	mu        sync.Mutex
	conn      *websocket.Conn
	conns     map[string]*relayConn
	connected bool
}

type relayConn struct {
	id       string
	host     *relayHost
	incoming chan relayPacket
	done     chan struct{}
	once     sync.Once
}

// relayWire is the relay's own JSON frame, not part of the E2EE application
// protocol.
type relayWire struct {
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

type relayPacket struct {
	data  []byte
	close bool
}

func newRelayHost(base, token, hostID string, log *slog.Logger, onConn func(string, *relayConn)) *relayHost {
	base = strings.TrimRight(base, "/")
	if !strings.HasSuffix(base, "/host") {
		base += "/host"
	}
	return &relayHost{
		url: base, token: token, hostID: hostID, log: log, onConn: onConn,
		conns: map[string]*relayConn{},
	}
}

// Run maintains the relay connection until ctx ends.
func (h *relayHost) Run(ctx context.Context) {
	backoff := time.Second
	for {
		if ctx.Err() != nil {
			return
		}
		err := h.session(ctx)
		h.mu.Lock()
		h.connected = false
		conns := h.conns
		h.conns = map[string]*relayConn{}
		h.mu.Unlock()
		for _, conn := range conns {
			conn.fail()
		}
		if ctx.Err() != nil {
			return
		}
		if err != nil {
			h.log.Warn("relay disconnected", "error", err)
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(backoff):
		}
		if backoff < 30*time.Second {
			backoff *= 2
		}
	}
}

func (h *relayHost) session(ctx context.Context) error {
	header := http.Header{}
	header.Set("Authorization", "Bearer "+h.token)
	dialCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(dialCtx, h.url, &websocket.DialOptions{
		HTTPHeader:      header,
		CompressionMode: websocket.CompressionDisabled,
	})
	if err != nil {
		return fmt.Errorf("dial relay: %w", err)
	}
	conn.SetReadLimit(2 << 20)
	h.mu.Lock()
	h.conn = conn
	h.connected = true
	h.mu.Unlock()
	defer func() {
		h.mu.Lock()
		if h.conn == conn {
			h.conn = nil
			h.connected = false
		}
		h.mu.Unlock()
		_ = conn.Close(websocket.StatusNormalClosure, "")
	}()

	if err := h.send(relayWire{T: "hello", HostID: h.hostID}); err != nil {
		return err
	}
	h.log.Info("relay connected", "url", h.url)

	ping := time.NewTicker(25 * time.Second)
	defer ping.Stop()
	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			case <-ping.C:
				if err := h.send(relayWire{T: "ping"}); err != nil {
					_ = conn.Close(websocket.StatusGoingAway, "ping failed")
					return
				}
			}
		}
	}()

	for {
		if ctx.Err() != nil {
			return nil
		}
		readCtx, readCancel := context.WithTimeout(ctx, 90*time.Second)
		frame, err := readRelayFrame(readCtx, conn)
		readCancel()
		if err != nil {
			return err
		}
		switch frame.T {
		case "presence":
			continue
		case "open":
			if frame.Conn == "" {
				continue
			}
			transport := &relayConn{
				id:       frame.Conn,
				host:     h,
				incoming: make(chan relayPacket, 256),
				done:     make(chan struct{}),
			}
			h.mu.Lock()
			h.conns[frame.Conn] = transport
			h.mu.Unlock()
			go h.onConn(frame.Conn, transport)
		case "data":
			raw, err := base64.StdEncoding.DecodeString(frame.Payload)
			if err != nil || len(raw) > 1<<20 {
				continue
			}
			h.mu.Lock()
			transport := h.conns[frame.Conn]
			h.mu.Unlock()
			if transport != nil {
				select {
				case transport.incoming <- relayPacket{data: raw}:
				default:
					transport.fail()
				}
			}
		case "close":
			h.mu.Lock()
			transport := h.conns[frame.Conn]
			delete(h.conns, frame.Conn)
			h.mu.Unlock()
			if transport != nil {
				transport.fail()
			}
		case "pong":
		case "error":
			h.log.Warn("relay error", "code", frame.Code, "message", frame.Message)
		}
	}
}

func (h *relayHost) send(frame relayWire) error {
	h.mu.Lock()
	conn := h.conn
	h.mu.Unlock()
	if conn == nil {
		return errors.New("relay not connected")
	}
	raw, err := json.Marshal(frame)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	return conn.Write(ctx, websocket.MessageText, raw)
}

func (h *relayHost) drop(connID string) {
	h.mu.Lock()
	delete(h.conns, connID)
	h.mu.Unlock()
}

func (h *relayHost) revoke(deviceID string) {
	_ = h.send(relayWire{T: "revoke", Device: deviceID})
}

func (h *relayHost) repair(deviceID string) {
	_ = h.send(relayWire{T: "repair", Device: deviceID})
}

func (h *relayHost) Connected() bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.connected
}

// Send implements remote.Transport for one routed device connection.
func (c *relayConn) Send(data []byte) error {
	if len(data) > 1<<20 {
		return errors.New("bridge: relay payload exceeds limit")
	}
	return c.host.send(relayWire{
		T: "data", Conn: c.id,
		Payload: base64.StdEncoding.EncodeToString(data),
	})
}

// Receive implements remote.Transport.
func (c *relayConn) Receive(ctx context.Context) ([]byte, error) {
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-c.done:
		return nil, errors.New("bridge: relay connection closed")
	case packet, ok := <-c.incoming:
		if !ok || packet.close {
			return nil, errors.New("bridge: relay connection closed")
		}
		return packet.data, nil
	}
}

// Close implements remote.Transport.
func (c *relayConn) Close() error {
	c.once.Do(func() {
		close(c.done)
		c.host.drop(c.id)
		_ = c.host.send(relayWire{T: "close", Conn: c.id})
	})
	return nil
}

func (c *relayConn) fail() {
	c.once.Do(func() { close(c.done) })
}

func readRelayFrame(ctx context.Context, conn *websocket.Conn) (relayWire, error) {
	_, raw, err := conn.Read(ctx)
	if err != nil {
		return relayWire{}, err
	}
	var frame relayWire
	if err := json.Unmarshal(raw, &frame); err != nil {
		return relayWire{}, err
	}
	return frame, nil
}
