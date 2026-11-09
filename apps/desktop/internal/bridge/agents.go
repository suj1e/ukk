package bridge

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/suj1e/ukk/apps/desktop/internal/agents"
)

// AgentService adapts the provider-neutral runtime to the bridge contract and
// publishes snapshots to local and remote subscribers.
type AgentService struct {
	runtime agents.Runtime
	server  *Server

	mu       sync.Mutex
	watching map[string]bool
}

// NewAgentService wires a runtime to a server.
func NewAgentService(runtime agents.Runtime, server *Server) *AgentService {
	return &AgentService{runtime: runtime, server: server, watching: map[string]bool{}}
}

// Start starts or returns a session.
func (s *AgentService) Start(ctx context.Context, params StartParams) (AgentSnapshot, error) {
	if params.Adapter != "" && params.Adapter != agents.ProviderNameOMP {
		return AgentSnapshot{}, &Error{Code: "unsupported", Message: "unknown adapter " + params.Adapter}
	}
	snapshot, err := s.runtime.Start(ctx, agents.StartRequest{
		SessionID: params.SessionID,
		Cwd:       params.Cwd,
		Model:     params.Model,
	})
	if err != nil {
		return AgentSnapshot{}, mapAgentError(err)
	}
	s.watch(params.SessionID)
	out := convertSnapshot(snapshot)
	s.publish(params.SessionID, out)
	return out, nil
}

// Snapshot reads one session.
func (s *AgentService) Snapshot(ctx context.Context, sessionID string) (AgentSnapshot, error) {
	snapshot, err := s.runtime.Snapshot(ctx, sessionID)
	if err != nil {
		return AgentSnapshot{}, mapAgentError(err)
	}
	return convertSnapshot(snapshot), nil
}

// List returns every session.
func (s *AgentService) List(context.Context) []AgentSnapshot {
	snapshots := s.runtime.List()
	out := make([]AgentSnapshot, 0, len(snapshots))
	for _, snapshot := range snapshots {
		out = append(out, convertSnapshot(snapshot))
	}
	return out
}

// Prompt sends user input.
func (s *AgentService) Prompt(ctx context.Context, params PromptParams) error {
	if err := s.runtime.Prompt(ctx, params.SessionID, params.Text); err != nil {
		return mapAgentError(err)
	}
	return nil
}

// Abort cancels the current turn.
func (s *AgentService) Abort(ctx context.Context, sessionID string) error {
	if err := s.runtime.Abort(ctx, sessionID); err != nil {
		return mapAgentError(err)
	}
	return nil
}

// Stop explicitly ends the provider process.
func (s *AgentService) Stop(ctx context.Context, sessionID string) error {
	if err := s.runtime.Stop(ctx, sessionID); err != nil {
		return mapAgentError(err)
	}
	return nil
}

// Approve answers a pending approval.
func (s *AgentService) Approve(ctx context.Context, params ApproveParams) error {
	decision := agents.Decision(params.Decision)
	if !decision.Valid() {
		return &Error{Code: "invalid_params", Message: "unknown decision"}
	}
	if err := s.runtime.Approve(ctx, params.SessionID, params.ApprovalID, decision, params.Note); err != nil {
		return mapAgentError(err)
	}
	return nil
}

// Subscribe is unused by the HTTP API (events arrive by SSE) but satisfies the
// Agents interface for embedding and tests.
func (s *AgentService) Subscribe(sessionID string) (<-chan AgentSnapshot, func()) {
	events := s.runtime.Events(sessionID)
	out := make(chan AgentSnapshot, 8)
	done := make(chan struct{})
	go func() {
		defer close(out)
		for {
			select {
			case <-done:
				return
			case event, ok := <-events:
				if !ok {
					return
				}
				if event.Kind == agents.EventClosed {
					return
				}
				snapshot, err := s.runtime.Snapshot(context.Background(), sessionID)
				if err != nil {
					continue
				}
				select {
				case out <- convertSnapshot(snapshot):
				case <-done:
					return
				}
			}
		}
	}()
	var once sync.Once
	return out, func() { once.Do(func() { close(done) }) }
}

func (s *AgentService) watch(sessionID string) {
	s.mu.Lock()
	if s.watching[sessionID] {
		s.mu.Unlock()
		return
	}
	s.watching[sessionID] = true
	s.mu.Unlock()

	events := s.runtime.Events(sessionID)
	go func() {
		for event := range events {
			if event.Kind == agents.EventClosed {
				break
			}
			snapshot, err := s.runtime.Snapshot(context.Background(), sessionID)
			if err != nil {
				continue
			}
			s.publish(sessionID, convertSnapshot(snapshot))
		}
		s.mu.Lock()
		delete(s.watching, sessionID)
		s.mu.Unlock()
	}()
}

func (s *AgentService) publish(sessionID string, snapshot AgentSnapshot) {
	if s.server == nil {
		return
	}
	copied := snapshot
	s.server.Publish(Event{
		Type:      EventUpdate,
		SessionID: sessionID,
		Agent:     &copied,
		AtMS:      time.Now().UnixMilli(),
	})
}

func mapAgentError(err error) error {
	var typed *agents.Error
	if errors.As(err, &typed) {
		return &Error{Code: string(typed.Code), Message: typed.Message}
	}
	return &Error{Code: "internal", Message: err.Error()}
}

func convertSnapshot(in agents.Snapshot) AgentSnapshot {
	out := AgentSnapshot{
		SessionID:         in.ID,
		Provider:          in.Provider,
		ProviderSessionID: in.ProviderSessionID,
		State:             string(in.State),
		Provenance:        in.Provenance,
		UpdatedMS:         in.UpdatedMS,
	}
	for _, turn := range in.Turns {
		converted := Turn{ID: turn.ID, State: string(turn.State), StartedMS: turn.StartedMS, EndedMS: turn.EndedMS}
		if turn.Error != nil {
			converted.Error = turn.Error.Message
		}
		out.Turns = append(out.Turns, converted)
	}
	for _, message := range in.Messages {
		out.Messages = append(out.Messages, Message{
			ID: message.ID, Role: string(message.Role), Text: message.Text,
			Final: message.Final, CreatedMS: message.CreatedMS,
		})
	}
	for _, tool := range in.Tools {
		out.Tools = append(out.Tools, Tool{
			ID: tool.ID, TurnID: tool.TurnID, Name: tool.Name,
			Input: tool.Input, Output: tool.Output, Status: string(tool.Status),
			IsError: tool.IsError, StartedMS: tool.StartedMS, EndedMS: tool.EndedMS,
		})
	}
	for _, approval := range in.PendingApprovals {
		out.PendingApprovals = append(out.PendingApprovals, Approval{
			ID: approval.ID, TurnID: approval.TurnID, ToolName: approval.ToolName,
			Input: approval.Input, State: string(approval.State), ExpiresMS: approval.ExpiresMS,
		})
	}
	if in.Usage.Reported {
		out.Usage = Usage{TotalTokens: in.Usage.TotalTokens}
	}
	if in.Error != nil {
		out.Error = in.Error.Message
	}
	return out
}

// MarshalSnapshot is a helper for tests and diagnostics.
func MarshalSnapshot(snapshot AgentSnapshot) (json.RawMessage, error) {
	raw, err := json.Marshal(snapshot)
	if err != nil {
		return nil, fmt.Errorf("bridge: encode snapshot: %w", err)
	}
	return raw, nil
}
