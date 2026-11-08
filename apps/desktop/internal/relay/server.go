package relay

import (
	"context"
	"crypto/subtle"
	"errors"
	"net"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
)

// Config configures the relay. OperatorToken is required and must be at least
// 32 bytes with at least eight distinct byte values.
type Config struct {
	OperatorToken string
	PingInterval  time.Duration
	IdleTimeout   time.Duration
	WriteTimeout  time.Duration
	HelloTimeout  time.Duration
	// OnEvent is called synchronously from routing goroutines. It must not
	// block or call back into the relay.
	OnEvent func(Event)
}

func (c Config) withDefaults() Config {
	if c.PingInterval <= 0 {
		c.PingInterval = 30 * time.Second
	}
	if c.IdleTimeout <= 0 {
		c.IdleTimeout = 90 * time.Second
	}
	if c.WriteTimeout <= 0 {
		c.WriteTimeout = 10 * time.Second
	}
	if c.HelloTimeout <= 0 {
		c.HelloTimeout = 10 * time.Second
	}
	return c
}

func validateOperatorToken(token string) error {
	raw := []byte(token)
	if len(raw) < 32 {
		return newError(CodeConfigInvalid, "operator token must be at least 32 bytes")
	}
	var seen [256]bool
	distinct := 0
	for _, b := range raw {
		if !seen[b] {
			seen[b] = true
			distinct++
		}
	}
	if distinct < 8 {
		return newError(CodeConfigInvalid, "operator token has insufficient diversity")
	}
	return nil
}

// EventKind is a relay lifecycle notification. Events never carry payloads.
type EventKind string

const (
	EventHostConnected      EventKind = "host_connected"
	EventHostDisconnected   EventKind = "host_disconnected"
	EventDeviceConnected    EventKind = "device_connected"
	EventDeviceDisconnected EventKind = "device_disconnected"
	EventConnOpened         EventKind = "conn_opened"
	EventConnClosed         EventKind = "conn_closed"
	EventDeviceRevoked      EventKind = "device_revoked"
	EventDeviceRepaired     EventKind = "device_repaired"
	EventRejected           EventKind = "rejected"
	EventQueueOverflow      EventKind = "queue_overflow"
	EventShutdown           EventKind = "shutdown"
)

// Event describes relay routing metadata only.
type Event struct {
	Kind     EventKind
	HostID   string
	DeviceID string
	ConnID   string
	Code     Code
}

// Server is a self-hostable control-plane relay. It routes opaque frames and
// never decodes or logs payloads. There is no persistence.
type Server struct {
	cfg   Config
	token []byte
	hub   *hub

	ctx    context.Context
	cancel context.CancelFunc

	mu         sync.Mutex
	httpServer *http.Server
	listener   net.Listener

	closed       atomic.Bool
	shutdownOnce sync.Once
}

// New validates configuration and creates a relay server.
func New(cfg Config) (*Server, error) {
	if err := validateOperatorToken(cfg.OperatorToken); err != nil {
		return nil, err
	}
	cfg = cfg.withDefaults()
	ctx, cancel := context.WithCancel(context.Background())
	s := &Server{
		cfg:    cfg,
		token:  []byte(cfg.OperatorToken),
		ctx:    ctx,
		cancel: cancel,
	}
	s.hub = newHub(s)
	return s, nil
}

// Handler returns the HTTP handler exposing /healthz, /host and /device.
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", s.handleHealth)
	mux.HandleFunc("/host", s.handleHost)
	mux.HandleFunc("/device", s.handleDevice)
	return mux
}

// Start serves until ctx is cancelled or Shutdown is called. It returns nil on
// a clean shutdown.
func (s *Server) Start(ctx context.Context, addr string) error {
	if s.closed.Load() {
		return newError(CodeClosed, "relay closed")
	}
	if addr == "" {
		return newError(CodeConfigInvalid, "listen address required")
	}
	listener, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}
	httpServer := &http.Server{Handler: s.Handler()}
	s.mu.Lock()
	if s.closed.Load() {
		s.mu.Unlock()
		_ = listener.Close()
		return newError(CodeClosed, "relay closed")
	}
	s.listener = listener
	s.httpServer = httpServer
	s.mu.Unlock()

	go func() {
		select {
		case <-ctx.Done():
			_ = s.Shutdown(context.Background())
		case <-s.ctx.Done():
		}
	}()

	err = httpServer.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

// Addr returns the listener address after Start, or "".
func (s *Server) Addr() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.listener == nil {
		return ""
	}
	return s.listener.Addr().String()
}

// Shutdown closes all peers and stops the HTTP server. It is idempotent.
func (s *Server) Shutdown(ctx context.Context) error {
	var err error
	s.shutdownOnce.Do(func() {
		s.closed.Store(true)
		s.cancel()
		s.hub.closeAll()
		s.mu.Lock()
		httpServer := s.httpServer
		s.mu.Unlock()
		if httpServer != nil {
			err = httpServer.Shutdown(ctx)
		}
	})
	return err
}

func (s *Server) emit(event Event) {
	if s.cfg.OnEvent != nil {
		s.cfg.OnEvent(event)
	}
}

func (s *Server) authorized(r *http.Request) bool {
	const prefix = "Bearer "
	header := r.Header.Get("Authorization")
	if !strings.HasPrefix(header, prefix) {
		return false
	}
	got := []byte(strings.TrimPrefix(header, prefix))
	return subtle.ConstantTimeCompare(got, s.token) == 1
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	_, _ = w.Write([]byte("ok\n"))
}

func (s *Server) handleHost(w http.ResponseWriter, r *http.Request) {
	if s.closed.Load() {
		http.Error(w, "relay closed", http.StatusServiceUnavailable)
		return
	}
	if !s.authorized(r) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"error":"unauthorized"}`))
		return
	}
	s.servePeer(w, r, roleHost)
}

func (s *Server) handleDevice(w http.ResponseWriter, r *http.Request) {
	if s.closed.Load() {
		http.Error(w, "relay closed", http.StatusServiceUnavailable)
		return
	}
	s.servePeer(w, r, roleDevice)
}

func (s *Server) servePeer(w http.ResponseWriter, r *http.Request, role peerRole) {
	ws, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	ws.SetReadLimit(MaxReadBytes)
	p := newPeer(s, ws, role)
	go p.writeLoop()
	registered := false
	defer func() {
		p.terminate(CodeClosed)
		if registered {
			s.hub.unregister(p)
		}
	}()

	helloCtx, cancel := context.WithTimeout(s.ctx, s.cfg.HelloTimeout)
	typ, raw, err := ws.Read(helloCtx)
	cancel()
	if err != nil {
		return
	}
	p.touch()
	if typ != websocket.MessageText {
		p.fail(newFatal(CodeFrameInvalid, "control frames must be text"))
		return
	}
	f, ferr := decodeFrame(raw)
	if ferr != nil {
		p.fail(ferr)
		return
	}
	if f.T != "hello" {
		p.fail(newFatal(CodeBadRequest, "first frame must be hello"))
		return
	}
	switch role {
	case roleHost:
		if !validID(f.HostID) || f.DeviceID != "" {
			p.fail(newFatal(CodeBadRequest, "hello requires host_id only"))
			return
		}
		if err := s.hub.addHost(p, f.HostID); err != nil {
			p.fail(err)
			return
		}
		registered = true
	case roleDevice:
		if !validID(f.HostID) || !validID(f.DeviceID) {
			p.fail(newFatal(CodeBadRequest, "hello requires host_id and device_id"))
			return
		}
		if err := s.hub.addDevice(p, f.HostID, f.DeviceID); err != nil {
			p.fail(err)
			return
		}
		registered = true
	default:
		p.fail(newFatal(CodeRoleMismatch, "unknown role"))
		return
	}

	for {
		typ, raw, err := ws.Read(s.ctx)
		if err != nil {
			return
		}
		p.touch()
		if typ != websocket.MessageText {
			p.fail(newFatal(CodeFrameInvalid, "control frames must be text"))
			return
		}
		f, ferr := decodeFrame(raw)
		if ferr != nil {
			p.fail(ferr)
			return
		}
		if f.T == "hello" {
			p.fail(newFatal(CodeDuplicateHello, "connection already registered"))
			return
		}
		if ferr := s.handleFrame(p, f); ferr != nil {
			p.sendError(ferr)
			if ferr.Fatal() {
				p.terminate(ferr.Code)
				return
			}
		}
	}
}

func (s *Server) handleFrame(p *peer, f frame) *Error {
	if p.role == roleHost {
		return s.handleHostFrame(p, f)
	}
	return s.handleDeviceFrame(p, f)
}

func (s *Server) handleHostFrame(p *peer, f frame) *Error {
	switch f.T {
	case "data":
		if !validID(f.Conn) {
			return newFatal(CodeBadRequest, "data requires conn")
		}
		if err := validatePayload(f.Payload); err != nil {
			return err
		}
		return s.hub.hostData(p, f.Conn, f.Payload)
	case "close":
		if !validID(f.Conn) {
			return newFatal(CodeBadRequest, "close requires conn")
		}
		return s.hub.closeFromHost(p, f.Conn)
	case "revoke":
		if !validID(f.Device) {
			return newFatal(CodeBadRequest, "revoke requires device")
		}
		s.hub.revokeDevice(p.hostID, f.Device)
		return nil
	case "repair":
		if !validID(f.Device) {
			return newFatal(CodeBadRequest, "repair requires device")
		}
		s.hub.repairDevice(p.hostID, f.Device)
		return nil
	case "open", "presence":
		return newFatal(CodeRoleMismatch, "host cannot send %s", f.T)
	default:
		return newFatal(CodeFrameInvalid, "unknown frame type")
	}
}

func (s *Server) handleDeviceFrame(p *peer, f frame) *Error {
	switch f.T {
	case "open":
		if !validID(f.Conn) {
			return newFatal(CodeBadRequest, "open requires conn")
		}
		return s.hub.openConn(p, f.Conn)
	case "data":
		if !validID(f.Conn) {
			return newFatal(CodeBadRequest, "data requires conn")
		}
		if err := validatePayload(f.Payload); err != nil {
			return err
		}
		return s.hub.deviceData(p, f.Conn, f.Payload)
	case "close":
		if !validID(f.Conn) {
			return newFatal(CodeBadRequest, "close requires conn")
		}
		return s.hub.closeFromDevice(p, f.Conn)
	case "revoke", "repair", "presence":
		return newFatal(CodeRoleMismatch, "device cannot send %s", f.T)
	default:
		return newFatal(CodeFrameInvalid, "unknown frame type")
	}
}

func (s *Server) enqueueFrame(p *peer, f frame) bool {
	raw, err := encodeFrame(f)
	if err != nil {
		return false
	}
	if !p.enqueue(raw) {
		go p.terminate(CodeQueueOverflow)
		s.emit(Event{Kind: EventQueueOverflow, HostID: p.hostID, DeviceID: p.deviceID, Code: CodeQueueOverflow})
		return false
	}
	return true
}

func (s *Server) sendPresenceToHost(host *peer, deviceID string, online bool) {
	if host == nil {
		return
	}
	value := online
	s.enqueueFrame(host, frame{T: "presence", DeviceID: deviceID, Online: &value})
}

func (s *Server) sendPresenceToDevice(device *peer, hostID string, online bool) {
	if device == nil {
		return
	}
	value := online
	s.enqueueFrame(device, frame{T: "presence", HostID: hostID, Online: &value})
}
