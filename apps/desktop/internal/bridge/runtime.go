package bridge

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"sync"
	"syscall"

	"github.com/suj1e/ukk/apps/desktop/internal/agents"
)

// Run starts the local bridge and blocks until ctx ends or it fails. Stopping
// the bridge explicitly ends the Agent children it owns; closing a Desktop
// window does not.
func Run(parent context.Context) error {
	dir, err := resolveDir()
	if err != nil {
		return err
	}
	config, err := LoadConfig(dir)
	if err != nil {
		return err
	}
	token, err := LocalToken(dir)
	if err != nil {
		return err
	}
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))

	runtime, err := agents.NewOMP(agents.Config{})
	if err != nil {
		return fmt.Errorf("bridge: start Agent runtime: %w", err)
	}
	mirror, err := OpenWorkspaceMirror(dir)
	if err != nil {
		_ = runtime.Close()
		return err
	}
	holder := &lateAgents{}
	server, err := NewServer(ServerOptions{Agents: holder, Workspace: mirror, Logger: logger})
	if err != nil {
		_ = runtime.Close()
		return err
	}
	holder.set(NewAgentService(runtime, server))

	remoteMgr, err := NewRemoteManager(config, dir, server, logger)
	if err != nil {
		_ = runtime.Close()
		return err
	}
	server.remote = remoteMgr
	screenMgr := NewScreenManager(config, logger)
	server.screen = screenMgr
	pluginSvc, err := NewPluginService(dir)
	if err != nil {
		_ = runtime.Close()
		return err
	}
	server.plugins = pluginSvc
	defer pluginSvc.Shutdown()
	defer func() {
		for _, session := range screenMgr.sessions {
			screenMgr.shutdownShare(session)
		}
	}()

	listener, endpoint, err := Listen(dir)
	if err != nil {
		_ = runtime.Close()
		return err
	}
	if err := SaveEndpoint(dir, endpoint); err != nil {
		listener.Close()
		_ = runtime.Close()
		return err
	}
	logger.Info("bridge listening", "network", endpoint.Network, "address", endpoint.Address)

	ctx, stop := signal.NotifyContext(parent, os.Interrupt, syscall.SIGTERM)
	defer stop()
	remoteMgr.Start(ctx)

	serveErr := Serve(ctx, listener, server.Handler(token), logger)
	server.Close()
	_ = runtime.Close()
	ClearEndpoint(dir)
	if serveErr != nil {
		return serveErr
	}
	return nil
}

func resolveDir() (string, error) {
	if override := os.Getenv("UKK_DATA_DIR"); override != "" {
		return override, nil
	}
	return DataDir()
}

// lateAgents lets the server be constructed before the AgentService that
// needs it, without changing the server's public contract.
type lateAgents struct {
	mu      sync.RWMutex
	service *AgentService
}

func (l *lateAgents) set(service *AgentService) {
	l.mu.Lock()
	l.service = service
	l.mu.Unlock()
}

func (l *lateAgents) get() (*AgentService, error) {
	l.mu.RLock()
	defer l.mu.RUnlock()
	if l.service == nil {
		return nil, &Error{Code: "unavailable", Message: "Agent runtime is starting"}
	}
	return l.service, nil
}

func (l *lateAgents) Start(ctx context.Context, params StartParams) (AgentSnapshot, error) {
	service, err := l.get()
	if err != nil {
		return AgentSnapshot{}, err
	}
	return service.Start(ctx, params)
}

func (l *lateAgents) Snapshot(ctx context.Context, sessionID string) (AgentSnapshot, error) {
	service, err := l.get()
	if err != nil {
		return AgentSnapshot{}, err
	}
	return service.Snapshot(ctx, sessionID)
}

func (l *lateAgents) List(ctx context.Context) []AgentSnapshot {
	service, err := l.get()
	if err != nil {
		return nil
	}
	return service.List(ctx)
}

func (l *lateAgents) Prompt(ctx context.Context, params PromptParams) error {
	service, err := l.get()
	if err != nil {
		return err
	}
	return service.Prompt(ctx, params)
}

func (l *lateAgents) Abort(ctx context.Context, sessionID string) error {
	service, err := l.get()
	if err != nil {
		return err
	}
	return service.Abort(ctx, sessionID)
}

func (l *lateAgents) Stop(ctx context.Context, sessionID string) error {
	service, err := l.get()
	if err != nil {
		return err
	}
	return service.Stop(ctx, sessionID)
}

func (l *lateAgents) Approve(ctx context.Context, params ApproveParams) error {
	service, err := l.get()
	if err != nil {
		return err
	}
	return service.Approve(ctx, params)
}

func (l *lateAgents) Subscribe(sessionID string) (<-chan AgentSnapshot, func()) {
	service, err := l.get()
	if err != nil {
		closed := make(chan AgentSnapshot)
		close(closed)
		return closed, func() {}
	}
	return service.Subscribe(sessionID)
}

// DecodeJSON is a small helper for tests and embedding callers.
func DecodeJSON(raw []byte, target any) error {
	if len(raw) == 0 {
		return errors.New("bridge: empty JSON")
	}
	return json.Unmarshal(raw, target)
}
