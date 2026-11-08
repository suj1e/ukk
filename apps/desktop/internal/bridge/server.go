package bridge

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
)

// Agents is the provider-neutral Agent surface the bridge exposes.
type Agents interface {
	Start(ctx context.Context, params StartParams) (AgentSnapshot, error)
	Snapshot(ctx context.Context, sessionID string) (AgentSnapshot, error)
	List(ctx context.Context) []AgentSnapshot
	Prompt(ctx context.Context, params PromptParams) error
	Abort(ctx context.Context, sessionID string) error
	Stop(ctx context.Context, sessionID string) error
	Approve(ctx context.Context, params ApproveParams) error
	Subscribe(sessionID string) (<-chan AgentSnapshot, func())
}

// Workspace is the metadata mirror remote clients read.
type Workspace interface {
	Snapshot() json.RawMessage
	Replace(json.RawMessage) error
}

// Remote is the pairing and device-trust surface.
type Remote interface {
	Status() Status
	Invite(ctx context.Context) (Invite, error)
	Revoke(ctx context.Context, deviceID string) error
	Grant(ctx context.Context, params GrantParams) error
	Confirm(ctx context.Context, params ConfirmParams) error
}

// Plugins is the extension runtime. Installing or enabling code is local-only;
// paired devices need the sensitive plugin.control grant.
type Plugins interface {
	List(ctx context.Context) []extensions.PluginStatus
	Install(ctx context.Context, params PluginInstallParams) (extensions.PluginStatus, error)
	Enable(ctx context.Context, id string) error
	Disable(id string) error
	Grant(plugin, capability, scope string) error
	Revoke(plugin, capability, scope string) error
	Invoke(ctx context.Context, params PluginInvokeParams) (extensions.InvokeResult, error)
}

// Screen is the Remote Screen media surface.
type Screen interface {
	Start(ctx context.Context, params ScreenStartParams) (ScreenSession, error)
	Answer(ctx context.Context, params ScreenAnswerParams) error
	Stop(ctx context.Context, sessionID string) error
	Input(ctx context.Context, params ScreenInputParams) error
}

// Authorizer decides whether the calling principal may run a method. The
// local Desktop authorizes every method; a remote device checks its live
// grants, expiry and revocation.
type Authorizer interface {
	Allow(ctx context.Context, method string, params json.RawMessage) error
}

// AllowAll is the local same-user authorizer.
type AllowAll struct{}

// Allow implements Authorizer.
func (AllowAll) Allow(context.Context, string, json.RawMessage) error { return nil }

// Denied is a typed authorization failure.
type Denied struct{ Reason string }

func (d *Denied) Error() string { return "bridge: " + d.Reason }

// Server serves the local API and dispatches the same commands for remote
// clients.
type Server struct {
	agents    Agents
	workspace Workspace
	remote    Remote
	screen    Screen
	plugins   Plugins
	log       *slog.Logger

	mu   sync.Mutex
	subs map[chan Event]struct{}
	done chan struct{}
	once sync.Once
}

// ServerOptions are the required collaborators. A nil Remote or Screen keeps
// those commands unavailable rather than pretending they work.
type ServerOptions struct {
	Agents    Agents
	Workspace Workspace
	Remote    Remote
	Screen    Screen
	Plugins   Plugins
	Logger    *slog.Logger
}

// NewServer validates and constructs a server.
func NewServer(opts ServerOptions) (*Server, error) {
	if opts.Agents == nil {
		return nil, errors.New("bridge: agents required")
	}
	if opts.Workspace == nil {
		return nil, errors.New("bridge: workspace mirror required")
	}
	logger := opts.Logger
	if logger == nil {
		logger = slog.Default()
	}
	return &Server{
		agents:    opts.Agents,
		workspace: opts.Workspace,
		remote:    opts.Remote,
		screen:    opts.Screen,
		plugins:   opts.Plugins,
		log:       logger,
		subs:      make(map[chan Event]struct{}),
		done:      make(chan struct{}),
	}, nil
}

// Close ends event subscriptions. It does not stop Agents; callers decide
// whether an explicit shutdown should.
func (s *Server) Close() {
	s.once.Do(func() {
		close(s.done)
		s.mu.Lock()
		for ch := range s.subs {
			close(ch)
		}
		s.subs = map[chan Event]struct{}{}
		s.mu.Unlock()
	})
}

// Publish broadcasts an event to SSE clients and remote transports.
func (s *Server) Publish(event Event) {
	if event.AtMS == 0 {
		event.AtMS = time.Now().UnixMilli()
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for ch := range s.subs {
		select {
		case ch <- event:
		default:
			// A slow client misses an update; the next snapshot is authoritative.
		}
	}
}

// Handler returns the local HTTP handler. Every route except /v1/health
// requires the bearer token.
func (s *Server) Handler(token string) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /v1/health", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, map[string]any{"ok": true, "version": Version})
	})
	mux.HandleFunc("POST /v1/command", s.authorized(token, s.handleCommand))
	mux.HandleFunc("GET /v1/events", s.authorized(token, s.handleEvents))
	return mux
}

func (s *Server) authorized(token string, next http.HandlerFunc) http.HandlerFunc {
	expected := []byte(token)
	return func(w http.ResponseWriter, r *http.Request) {
		raw := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
		if len(raw) == 0 || subtle.ConstantTimeCompare([]byte(raw), expected) != 1 {
			writeJSON(w, http.StatusUnauthorized, Response{Error: &Error{Code: "unauthorized", Message: "invalid local token"}})
			return
		}
		next(w, r)
	}
}

func (s *Server) handleCommand(w http.ResponseWriter, r *http.Request) {
	r.Body = http.MaxBytesReader(w, r.Body, 1<<20)
	raw, err := io.ReadAll(r.Body)
	if err != nil {
		writeJSON(w, http.StatusBadRequest, Response{Error: &Error{Code: "too_large", Message: "request exceeds 1 MiB"}})
		return
	}
	var request Request
	decoder := json.NewDecoder(strings.NewReader(string(raw)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&request); err != nil {
		writeJSON(w, http.StatusBadRequest, Response{Error: &Error{Code: "invalid_request", Message: err.Error()}})
		return
	}
	if request.ID == "" || request.Method == "" {
		writeJSON(w, http.StatusBadRequest, Response{ID: request.ID, Error: &Error{Code: "invalid_request", Message: "id and method are required"}})
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 45*time.Second)
	defer cancel()
	result, err := s.Dispatch(ctx, request.Method, request.Params, AllowAll{})
	response := Response{ID: request.ID}
	if err != nil {
		response.Error = toWireError(err)
	} else if result != nil {
		encoded, encodeErr := json.Marshal(result)
		if encodeErr != nil {
			response.Error = &Error{Code: "internal", Message: "could not encode result"}
		} else {
			response.Result = encoded
		}
	}
	status := http.StatusOK
	if response.Error != nil && response.Error.Code == "unauthorized" {
		status = http.StatusForbidden
	}
	writeJSON(w, status, response)
}

func (s *Server) handleEvents(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		writeJSON(w, http.StatusInternalServerError, Response{Error: &Error{Code: "streaming_unsupported", Message: "response does not support streaming"}})
		return
	}
	sessionID := r.URL.Query().Get("session_id")
	ch := make(chan Event, 32)
	s.mu.Lock()
	s.subs[ch] = struct{}{}
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		delete(s.subs, ch)
		s.mu.Unlock()
	}()
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.WriteHeader(http.StatusOK)
	flusher.Flush()

	keepalive := time.NewTicker(15 * time.Second)
	defer keepalive.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case <-s.done:
			return
		case <-keepalive.C:
			fmt.Fprint(w, ": keepalive\n\n")
			flusher.Flush()
		case event := <-ch:
			if sessionID != "" && event.SessionID != "" && event.SessionID != sessionID {
				continue
			}
			encoded, err := json.Marshal(event)
			if err != nil {
				continue
			}
			fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event.Type, encoded)
			flusher.Flush()
		}
	}
}

// Dispatch validates parameters and runs one command. Remote transports pass
// their device authorizer; the local API passes AllowAll.
func (s *Server) Dispatch(ctx context.Context, method string, params json.RawMessage, authz Authorizer) (any, error) {
	if authz == nil {
		return nil, &Error{Code: "unauthorized", Message: "no authorization context"}
	}
	if err := authz.Allow(ctx, method, params); err != nil {
		var denied *Denied
		if errors.As(err, &denied) {
			return nil, &Error{Code: "forbidden", Message: denied.Reason}
		}
		return nil, &Error{Code: "forbidden", Message: err.Error()}
	}
	switch method {
	case MethodHealth:
		return map[string]any{"ok": true, "version": Version}, nil

	case MethodWorkspaceGet:
		return json.RawMessage(s.workspace.Snapshot()), nil

	case MethodWorkspaceSet:
		var request WorkspaceParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if len(request.Snapshot) == 0 {
			return nil, &Error{Code: "invalid_params", Message: "snapshot required"}
		}
		if err := s.workspace.Replace(request.Snapshot); err != nil {
			return nil, &Error{Code: "invalid_snapshot", Message: err.Error()}
		}
		return map[string]bool{"ok": true}, nil

	case MethodAgentStart:
		var request StartParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if request.SessionID == "" || request.Cwd == "" {
			return nil, &Error{Code: "invalid_params", Message: "session_id and cwd are required"}
		}
		return s.agents.Start(ctx, request)

	case MethodAgentSnapshot:
		var request SessionParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return s.agents.Snapshot(ctx, request.SessionID)

	case MethodAgentList:
		return map[string]any{"sessions": s.agents.List(ctx)}, nil

	case MethodAgentPrompt:
		var request PromptParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if strings.TrimSpace(request.Text) == "" {
			return nil, &Error{Code: "invalid_params", Message: "text required"}
		}
		return map[string]bool{"ok": true}, s.agents.Prompt(ctx, request)

	case MethodAgentAbort, MethodAgentStop:
		var request SessionParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if method == MethodAgentAbort {
			return map[string]bool{"ok": true}, s.agents.Abort(ctx, request.SessionID)
		}
		return map[string]bool{"ok": true}, s.agents.Stop(ctx, request.SessionID)

	case MethodAgentApprove:
		var request ApproveParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		switch request.Decision {
		case "approve", "deny":
		default:
			return nil, &Error{Code: "invalid_params", Message: "decision must be approve or deny"}
		}
		return map[string]bool{"ok": true}, s.agents.Approve(ctx, request)

	case MethodPluginList:
		if s.plugins == nil {
			return nil, &Error{Code: "unavailable", Message: "plugins are not configured"}
		}
		return map[string]any{"plugins": s.plugins.List(ctx)}, nil

	case MethodPluginInstall:
		if s.plugins == nil {
			return nil, &Error{Code: "unavailable", Message: "plugins are not configured"}
		}
		var request PluginInstallParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return s.plugins.Install(ctx, request)

	case MethodPluginEnable, MethodPluginDisable:
		if s.plugins == nil {
			return nil, &Error{Code: "unavailable", Message: "plugins are not configured"}
		}
		var request PluginParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if method == MethodPluginEnable {
			return map[string]bool{"ok": true}, s.plugins.Enable(ctx, request.Plugin)
		}
		return map[string]bool{"ok": true}, s.plugins.Disable(request.Plugin)

	case MethodPluginGrant, MethodPluginRevoke:
		if s.plugins == nil {
			return nil, &Error{Code: "unavailable", Message: "plugins are not configured"}
		}
		var request PluginGrantParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if method == MethodPluginGrant {
			return map[string]bool{"ok": true}, s.plugins.Grant(request.Plugin, request.Capability, request.Scope)
		}
		return map[string]bool{"ok": true}, s.plugins.Revoke(request.Plugin, request.Capability, request.Scope)

	case MethodPluginInvoke:
		if s.plugins == nil {
			return nil, &Error{Code: "unavailable", Message: "plugins are not configured"}
		}
		var request PluginInvokeParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return s.plugins.Invoke(ctx, request)

	case MethodRemoteStatus:
		if s.remote == nil {
			return nil, &Error{Code: "unavailable", Message: "remote access is not configured"}
		}
		return s.remote.Status(), nil

	case MethodRemoteInvite:
		if s.remote == nil {
			return nil, &Error{Code: "unavailable", Message: "remote access is not configured"}
		}
		return s.remote.Invite(ctx)

	case MethodRemoteRevoke:
		if s.remote == nil {
			return nil, &Error{Code: "unavailable", Message: "remote access is not configured"}
		}
		var request RevokeParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return map[string]bool{"ok": true}, s.remote.Revoke(ctx, request.DeviceID)

	case MethodRemoteGrant:
		if s.remote == nil {
			return nil, &Error{Code: "unavailable", Message: "remote access is not configured"}
		}
		var request GrantParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return map[string]bool{"ok": true}, s.remote.Grant(ctx, request)

	case MethodRemoteConfirm:
		if s.remote == nil {
			return nil, &Error{Code: "unavailable", Message: "remote access is not configured"}
		}
		var request ConfirmParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return map[string]bool{"ok": true}, s.remote.Confirm(ctx, request)

	case MethodScreenStart:
		if s.screen == nil {
			return nil, &Error{Code: "unavailable", Message: "Remote Screen is unavailable on this host"}
		}
		var request ScreenStartParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		if !request.View {
			return nil, &Error{Code: "invalid_params", Message: "screen view grant required"}
		}
		return s.screen.Start(ctx, request)

	case MethodScreenAnswer:
		if s.screen == nil {
			return nil, &Error{Code: "unavailable", Message: "Remote Screen is unavailable on this host"}
		}
		var request ScreenAnswerParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return map[string]bool{"ok": true}, s.screen.Answer(ctx, request)

	case MethodScreenStop:
		if s.screen == nil {
			return nil, &Error{Code: "unavailable", Message: "Remote Screen is unavailable on this host"}
		}
		var request SessionParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return map[string]bool{"ok": true}, s.screen.Stop(ctx, request.SessionID)

	case MethodScreenInput:
		if s.screen == nil {
			return nil, &Error{Code: "unavailable", Message: "Remote Screen is unavailable on this host"}
		}
		var request ScreenInputParams
		if err := DecodeParams(params, &request); err != nil {
			return nil, err
		}
		return map[string]bool{"ok": true}, s.screen.Input(ctx, request)

	default:
		return nil, &Error{Code: "unknown_method", Message: "unknown method " + method}
	}
}

func toWireError(err error) *Error {
	var typed *Error
	if errors.As(err, &typed) {
		return typed
	}
	var denied *Denied
	if errors.As(err, &denied) {
		return &Error{Code: "forbidden", Message: denied.Reason}
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return &Error{Code: "timeout", Message: "command timed out"}
	}
	return &Error{Code: "internal", Message: err.Error()}
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

// Serve runs the HTTP server on the private listener until ctx ends.
func Serve(ctx context.Context, listener net.Listener, handler http.Handler, log *slog.Logger) error {
	server := &http.Server{
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      0, // SSE
		IdleTimeout:       120 * time.Second,
		MaxHeaderBytes:    64 << 10,
	}
	go func() {
		<-ctx.Done()
		shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdown)
	}()
	err := server.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

// SubscribeEvents returns a channel of pushed events for embedding transports.
func (s *Server) SubscribeEvents() (<-chan Event, func()) {
	ch := make(chan Event, 64)
	s.mu.Lock()
	s.subs[ch] = struct{}{}
	s.mu.Unlock()
	var once sync.Once
	return ch, func() {
		once.Do(func() {
			s.mu.Lock()
			delete(s.subs, ch)
			s.mu.Unlock()
		})
	}
}
