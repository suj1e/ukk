// Package agents defines the provider-neutral Agent runtime contract shared by
// the Desktop, the bridge, and (through the bridge) iOS, and implements the
// omp adapter on top of `omp --mode rpc`.
//
// The contract is deliberately small and JSON-stable: every field that crosses
// the bridge is tagged, states are a closed vocabulary, and facts the provider
// did not report stay explicitly unknown instead of being inferred. In
// particular, terminal bytes are never interpreted as approvals or tool
// execution.
package agents

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
)

// ProvenanceOMPRPC labels every snapshot produced by the omp RPC adapter.
const ProvenanceOMPRPC = "omp-rpc"

// ProviderNameOMP is the provider name reported by the omp adapter.
const ProviderNameOMP = "omp"

// State is the lifecycle state of a Session or Turn.
type State string

// The closed state vocabulary. "unknown" is used whenever the provider has not
// reported enough information to decide; it is never replaced by a guess.
const (
	StateIdle      State = "idle"
	StateRunning   State = "running"
	StateBlocked   State = "blocked"
	StateDone      State = "done"
	StateFailed    State = "failed"
	StateCancelled State = "cancelled"
	StateUnknown   State = "unknown"
)

// Valid reports whether s belongs to the closed state vocabulary.
func (s State) Valid() bool {
	switch s {
	case StateIdle, StateRunning, StateBlocked, StateDone, StateFailed, StateCancelled, StateUnknown:
		return true
	}
	return false
}

// Role is the author role of a Message.
type Role string

// Message roles.
const (
	RoleUser      Role = "user"
	RoleAssistant Role = "assistant"
	RoleSystem    Role = "system"
	RoleTool      Role = "tool"
	RoleUnknown   Role = "unknown"
)

// ToolStatus is the lifecycle of one tool call.
type ToolStatus string

// Tool call statuses.
const (
	ToolPending   ToolStatus = "pending"
	ToolRunning   ToolStatus = "running"
	ToolCompleted ToolStatus = "completed"
	ToolFailed    ToolStatus = "failed"
	ToolUnknown   ToolStatus = "unknown"
)

// ApprovalState is the lifecycle of one approval request.
type ApprovalState string

// Approval states.
const (
	ApprovalPending   ApprovalState = "pending"
	ApprovalApproved  ApprovalState = "approved"
	ApprovalDenied    ApprovalState = "denied"
	ApprovalCancelled ApprovalState = "cancelled"
	ApprovalExpired   ApprovalState = "expired"
)

// Decision is the caller's answer to an approval request.
type Decision string

// Approval decisions.
const (
	DecisionApprove Decision = "approve"
	DecisionDeny    Decision = "deny"
	DecisionCancel  Decision = "cancel"
)

// Valid reports whether d is a supported approval decision.
func (d Decision) Valid() bool {
	switch d {
	case DecisionApprove, DecisionDeny, DecisionCancel:
		return true
	}
	return false
}

// EventKind discriminates an Event payload. Only the payload matching Kind is
// populated.
type EventKind string

// Event kinds. A "closed" event is always the last event on a session channel
// before the channel is closed.
const (
	EventSnapshot   EventKind = "snapshot"
	EventMessage    EventKind = "message"
	EventTool       EventKind = "tool"
	EventApproval   EventKind = "approval"
	EventUsage      EventKind = "usage"
	EventError      EventKind = "error"
	EventDiagnostic EventKind = "diagnostic"
	EventClosed     EventKind = "closed"
)

// ErrorCode is a stable machine-readable failure kind.
type ErrorCode string

// Stable error codes. Callers branch on these, never on message text.
const (
	CodeInvalidArgument  ErrorCode = "invalid_argument"
	CodeSessionNotFound  ErrorCode = "session_not_found"
	CodeSessionExists    ErrorCode = "session_exists"
	CodeLimitExceeded    ErrorCode = "limit_exceeded"
	CodeUnsupported      ErrorCode = "unsupported"
	CodeNotRunning       ErrorCode = "not_running"
	CodeBusy             ErrorCode = "busy"
	CodeStartupFailed    ErrorCode = "startup_failed"
	CodeProtocol         ErrorCode = "protocol"
	CodeTransport        ErrorCode = "transport"
	CodeTimeout          ErrorCode = "timeout"
	CodeChildExited      ErrorCode = "child_exited"
	CodeRejected         ErrorCode = "rejected"
	CodeClosed           ErrorCode = "closed"
	CodeApprovalNotFound ErrorCode = "approval_not_found"
	CodeApprovalSettled  ErrorCode = "approval_settled"
	CodeDiagnostic       ErrorCode = "diagnostic"
	CodeUIUnsupported    ErrorCode = "ui.unsupported"
	CodeTurnFailed       ErrorCode = "turn_failed"
)

// Error is the typed error carried by the contract and by every returned
// error. Messages never contain credentials: provider-supplied text is
// redacted and bounded before it is stored.
type Error struct {
	Code    ErrorCode `json:"code"`
	Message string    `json:"message"`
}

// Error implements the error interface.
func (e *Error) Error() string {
	if e == nil {
		return "<nil>"
	}
	return string(e.Code) + ": " + e.Message
}

// NewError builds a typed error with a bounded, redacted message.
func NewError(code ErrorCode, format string, args ...any) *Error {
	return &Error{Code: code, Message: boundedMessage(fmt.Sprintf(format, args...), 2048)}
}

// CodeOf returns the stable error code of err, or "" when err is not a typed
// agents error.
func CodeOf(err error) ErrorCode {
	var typed *Error
	if errors.As(err, &typed) {
		return typed.Code
	}
	return ""
}

// Capabilities describes what a provider can report. A false field means the
// runtime returns a typed unsupported error instead of approximating it.
type Capabilities struct {
	StructuredTurns bool `json:"structuredTurns"`
	Tools           bool `json:"tools"`
	Approvals       bool `json:"approvals"`
	Usage           bool `json:"usage"`
	Resume          bool `json:"resume"`
	Steer           bool `json:"steer"`
}

// Provider identifies a runtime implementation and its capabilities.
type Provider struct {
	Name         string       `json:"name"`
	Capabilities Capabilities `json:"capabilities"`
}

// StartRequest starts one Agent session. SessionID is the product identity; the
// runtime never reuses it for a second live child.
type StartRequest struct {
	SessionID string            `json:"sessionId"`
	Cwd       string            `json:"cwd,omitempty"`
	Model     string            `json:"model,omitempty"`
	Profile   string            `json:"profile,omitempty"`
	Env       map[string]string `json:"env,omitempty"`
}

// Message is one visible conversation message. Text is answer text only;
// thinking is never merged into it.
type Message struct {
	ID        string `json:"id"`
	Role      Role   `json:"role"`
	Text      string `json:"text"`
	Final     bool   `json:"final"`
	CreatedMS int64  `json:"createdMs"`
}

// Turn is one structured agent turn.
type Turn struct {
	ID        string `json:"id"`
	State     State  `json:"state"`
	StartedMS int64  `json:"startedMs"`
	EndedMS   int64  `json:"endedMs,omitempty"`
	Error     *Error `json:"error,omitempty"`
}

// ToolCall is one tool execution reported by the provider. Input and Output are
// raw provider JSON and may be nil when the provider did not report them.
type ToolCall struct {
	ID        string          `json:"id"`
	TurnID    string          `json:"turnId,omitempty"`
	Name      string          `json:"name"`
	Input     json.RawMessage `json:"input,omitempty"`
	Output    json.RawMessage `json:"output,omitempty"`
	Status    ToolStatus      `json:"status"`
	StartedMS int64           `json:"startedMs,omitempty"`
	EndedMS   int64           `json:"endedMs,omitempty"`
	IsError   bool            `json:"isError"`
}

// Approval is one pending provider approval request. ToolName and Input are
// populated only when the provider reported them structurally; otherwise they
// stay empty/nil and the approval is still answerable by ID.
type Approval struct {
	ID        string          `json:"id"`
	TurnID    string          `json:"turnId,omitempty"`
	ToolName  string          `json:"toolName,omitempty"`
	Input     json.RawMessage `json:"input,omitempty"`
	State     ApprovalState   `json:"state"`
	ExpiresMS int64           `json:"expiresMs,omitempty"`
}

// Usage is the most recent provider-reported token and cost record for the
// session. Reported=false means no provider usage was observed; every numeric
// field must then be ignored. It is never estimated from text or timing.
type Usage struct {
	Reported         bool    `json:"reported"`
	InputTokens      int64   `json:"inputTokens"`
	OutputTokens     int64   `json:"outputTokens"`
	CacheReadTokens  int64   `json:"cacheReadTokens"`
	CacheWriteTokens int64   `json:"cacheWriteTokens"`
	TotalTokens      int64   `json:"totalTokens"`
	ContextTokens    int64   `json:"contextTokens,omitempty"`
	ReasoningTokens  int64   `json:"reasoningTokens,omitempty"`
	CostUSD          float64 `json:"costUsd"`
}

// Snapshot is the complete renderable state of one session.
type Snapshot struct {
	Provider          string     `json:"provider"`
	ProviderSessionID string     `json:"providerSessionId,omitempty"`
	State             State      `json:"state"`
	Turns             []Turn     `json:"turns"`
	Messages          []Message  `json:"messages"`
	Tools             []ToolCall `json:"tools"`
	PendingApprovals  []Approval `json:"pendingApprovals"`
	Usage             Usage      `json:"usage"`
	Error             *Error     `json:"error,omitempty"`
	Provenance        string     `json:"provenance"`
	UpdatedMS         int64      `json:"updatedMs"`
}

// Event is one ordered update for a session. Sequence is monotonic per session
// and starts at 1.
type Event struct {
	Kind      EventKind `json:"kind"`
	SessionID string    `json:"sessionId"`
	Sequence  uint64    `json:"sequence"`
	Snapshot  *Snapshot `json:"snapshot,omitempty"`
	Message   *Message  `json:"message,omitempty"`
	Tool      *ToolCall `json:"tool,omitempty"`
	Approval  *Approval `json:"approval,omitempty"`
	Usage     *Usage    `json:"usage,omitempty"`
	Error     *Error    `json:"error,omitempty"`
}

// Runtime is the provider-neutral Agent runtime. Implementations must honor
// context cancellation, keep a bounded event history, and never let a
// subscriber disconnect affect a running child.
type Runtime interface {
	// Provider reports the provider identity and capabilities.
	Provider() Provider
	// Start starts a session. Starting an already running session with the same
	// ID is idempotent and returns its current snapshot.
	Start(ctx context.Context, req StartRequest) (Snapshot, error)
	// Snapshot returns the current state of a session.
	Snapshot(ctx context.Context, sessionID string) (Snapshot, error)
	// Prompt admits one prompt. It returns after the provider acknowledges
	// admission; the turn completes through Events.
	Prompt(ctx context.Context, sessionID, text string) error
	// Steer queues a steering message when Capabilities.Steer is true.
	Steer(ctx context.Context, sessionID, text string) error
	// Abort aborts the current run.
	Abort(ctx context.Context, sessionID string) error
	// Stop ends the provider program explicitly and reaps it. It is idempotent.
	Stop(ctx context.Context, sessionID string) error
	// Approve answers one pending approval.
	Approve(ctx context.Context, sessionID, approvalID string, decision Decision, note string) error
	// Resume reattaches to a provider session when Capabilities.Resume is true.
	// Otherwise it returns a typed CodeUnsupported error.
	Resume(ctx context.Context, sessionID, providerSessionID string) (Snapshot, error)
	// Events returns the session event stream. The channel closes after the
	// session ends; an unknown session yields an already-closed channel.
	Events(sessionID string) <-chan Event
	// List returns snapshots of every known session, ordered by session ID.
	List() []Snapshot
	// Close stops every session. It is idempotent.
	Close() error
}
