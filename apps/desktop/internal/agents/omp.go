package agents

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// Adapter defaults. They mirror the previously verified omp client behaviour
// while keeping every queue and history bounded.
const (
	defaultMaxSessions     = 4
	defaultStartupTimeout  = 20 * time.Second
	defaultRequestTimeout  = 15 * time.Second
	defaultShutdownGrace   = 3 * time.Second
	defaultEventHistory    = 512
	defaultSubscriberQueue = 256
	defaultMaxStderrLines  = 64
	stateQueryTimeout      = 2 * time.Second

	maxMessages          = 1000
	maxTools             = 500
	maxTurns             = 200
	maxApprovals         = 64
	maxUnknownEventTypes = 16
	maxSubscribers       = 32
	maxSessionIDLen      = 256
	maxEnvBytes          = 64 << 10
)

// Config configures the omp runtime. The zero value uses omp on PATH with
// `--mode rpc --no-session`.
type Config struct {
	// Binary is the omp executable. Empty means "omp".
	Binary string
	// Args replaces the default `--mode rpc --no-session` argument list.
	Args []string
	// MaxSessions bounds live children. The default is 4.
	MaxSessions int
	// StartupTimeout bounds the ready handshake.
	StartupTimeout time.Duration
	// RequestTimeout bounds one command acknowledgement.
	RequestTimeout time.Duration
	// ShutdownGrace is the grace period between closing stdin and killing.
	ShutdownGrace time.Duration
	// MaxFrameBytes is the configured inbound frame ceiling.
	MaxFrameBytes int
	// MaxStderrLines bounds the retained, redacted stderr tail.
	MaxStderrLines int
	// EventHistory bounds the replayable per-session event history.
	EventHistory int
	// SubscriberQueue bounds each subscriber's buffered queue.
	SubscriberQueue int
}

func (c Config) normalized() (Config, error) {
	if c.Binary == "" {
		c.Binary = "omp"
	}
	if c.Args == nil {
		c.Args = []string{"--mode", "rpc", "--no-session"}
	}
	if c.MaxSessions == 0 {
		c.MaxSessions = defaultMaxSessions
	}
	if c.MaxSessions < 1 {
		return Config{}, NewError(CodeInvalidArgument, "MaxSessions must be at least 1")
	}
	if c.MaxSessions > 64 {
		return Config{}, NewError(CodeInvalidArgument, "MaxSessions must be at most 64")
	}
	if c.StartupTimeout <= 0 {
		c.StartupTimeout = defaultStartupTimeout
	}
	if c.RequestTimeout <= 0 {
		c.RequestTimeout = defaultRequestTimeout
	}
	if c.ShutdownGrace <= 0 {
		c.ShutdownGrace = defaultShutdownGrace
	}
	if c.MaxFrameBytes == 0 {
		c.MaxFrameBytes = defaultMaxFrameBytes
	}
	if c.MaxFrameBytes < minFrameBytes || c.MaxFrameBytes > absoluteMaxFrameBytes {
		return Config{}, NewError(CodeInvalidArgument, "MaxFrameBytes must be between %d and %d", minFrameBytes, absoluteMaxFrameBytes)
	}
	if c.MaxStderrLines <= 0 {
		c.MaxStderrLines = defaultMaxStderrLines
	}
	if c.EventHistory <= 0 {
		c.EventHistory = defaultEventHistory
	}
	if c.SubscriberQueue <= 0 {
		c.SubscriberQueue = defaultSubscriberQueue
	}
	return c, nil
}

// OMP is the omp `--mode rpc` implementation of Runtime.
type OMP struct {
	cfg Config

	mu       sync.Mutex
	sessions map[string]*ompSession
	closed   bool

	closeOnce sync.Once
	closeErr  error
}

// NewOMP validates cfg and returns a runtime with no sessions.
func NewOMP(cfg Config) (*OMP, error) {
	normalized, err := cfg.normalized()
	if err != nil {
		return nil, err
	}
	return &OMP{cfg: normalized, sessions: make(map[string]*ompSession)}, nil
}

// Provider implements Runtime.
func (o *OMP) Provider() Provider {
	return Provider{
		Name: ProviderNameOMP,
		Capabilities: Capabilities{
			StructuredTurns: true,
			Tools:           true,
			Approvals:       true,
			Usage:           true,
			Resume:          false,
			Steer:           true,
		},
	}
}

// Start implements Runtime. Admission (limit check, spawn, insert) is
// serialized by the runtime mutex so concurrent Starts cannot exceed
// MaxSessions or create two children for one SessionID.
func (o *OMP) Start(ctx context.Context, req StartRequest) (Snapshot, error) {
	if err := ctx.Err(); err != nil {
		return Snapshot{}, err
	}
	if err := validateStartRequest(req); err != nil {
		return Snapshot{}, err
	}

	o.mu.Lock()
	if o.closed {
		o.mu.Unlock()
		return Snapshot{}, NewError(CodeClosed, "the omp runtime is closed")
	}
	if existing, ok := o.sessions[req.SessionID]; ok && existing.live() {
		snapshot := existing.snapshot()
		o.mu.Unlock()
		return snapshot, nil
	}
	if o.liveLocked() >= o.cfg.MaxSessions {
		o.mu.Unlock()
		return Snapshot{}, NewError(CodeLimitExceeded, "at most %d omp sessions can run at once", o.cfg.MaxSessions)
	}

	// A previous record for this ID is dead; settle it before replacing it.
	if previous, ok := o.sessions[req.SessionID]; ok {
		previous.finish(StateFailed, previous.exitError())
	}
	session, err := o.startLocked(ctx, req)
	if err != nil {
		o.mu.Unlock()
		return Snapshot{}, err
	}
	o.sessions[req.SessionID] = session
	o.mu.Unlock()

	// The session id is part of the snapshot contract; ask omp for it. A
	// provider that does not answer leaves it explicitly empty rather than
	// inventing one.
	session.fetchState()
	return session.snapshot(), nil
}

func (o *OMP) startLocked(ctx context.Context, req StartRequest) (*ompSession, error) {
	session := newOMPSession(o.cfg, req)
	process, err := startProcess(processConfig{
		binary:    o.cfg.Binary,
		args:      startArgs(o.cfg.Args, req),
		cwd:       req.Cwd,
		env:       req.Env,
		frameCap:  o.cfg.MaxFrameBytes,
		onLine:    session.handleLine,
		onStderr:  session.onStderr,
		onFailure: session.fail,
	})
	if err != nil {
		return nil, err
	}
	session.proc = process
	process.startReaders()

	timer := time.NewTimer(o.cfg.StartupTimeout)
	defer timer.Stop()
	select {
	case <-session.readyCh:
		if session.readyErr != nil {
			_ = process.terminate(0)
			return nil, session.readyErr
		}
		session.markReady()
		return session, nil
	case <-ctx.Done():
		_ = process.terminate(0)
		return nil, ctx.Err()
	case <-timer.C:
		stderr := session.stderrTail()
		_ = process.terminate(0)
		suffix := ""
		if len(stderr) > 0 {
			suffix = " (stderr: " + strings.Join(stderr, " | ") + ")"
		}
		return nil, NewError(CodeTimeout, "omp did not become ready within %s%s", o.cfg.StartupTimeout, suffix)
	case <-process.done:
		<-session.readyCh
		if session.readyErr != nil {
			return nil, session.readyErr
		}
		return nil, NewError(CodeChildExited, "omp exited before the ready handshake")
	}
}

// Snapshot implements Runtime.
func (o *OMP) Snapshot(ctx context.Context, sessionID string) (Snapshot, error) {
	if err := ctx.Err(); err != nil {
		return Snapshot{}, err
	}
	session, err := o.session(sessionID)
	if err != nil {
		return Snapshot{}, err
	}
	return session.snapshot(), nil
}

// Prompt implements Runtime.
func (o *OMP) Prompt(ctx context.Context, sessionID, text string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if strings.TrimSpace(text) == "" {
		return NewError(CodeInvalidArgument, "prompt text is empty")
	}
	if len(text) > maxPromptBytes {
		return NewError(CodeInvalidArgument, "prompt exceeds the %d-byte limit", maxPromptBytes)
	}
	session, err := o.session(sessionID)
	if err != nil {
		return err
	}
	turnID, err := session.beginPrompt(nowMS(), text)
	if err != nil {
		return err
	}
	if _, err := session.request(ctx, "prompt", map[string]any{"message": text}, o.cfg.RequestTimeout); err != nil {
		session.failTurn(turnID, err)
		return err
	}
	return nil
}

// Steer implements Runtime.
func (o *OMP) Steer(ctx context.Context, sessionID, text string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if strings.TrimSpace(text) == "" {
		return NewError(CodeInvalidArgument, "steer text is empty")
	}
	session, err := o.session(sessionID)
	if err != nil {
		return err
	}
	_, err = session.request(ctx, "steer", map[string]any{"message": text}, o.cfg.RequestTimeout)
	return err
}

// Abort implements Runtime.
func (o *OMP) Abort(ctx context.Context, sessionID string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	session, err := o.session(sessionID)
	if err != nil {
		return err
	}
	session.mu.Lock()
	if session.closed {
		session.mu.Unlock()
		return session.exitErrorLocked()
	}
	if session.turnActive {
		session.abortRequested = true
	}
	session.mu.Unlock()

	if _, err := session.request(ctx, "abort", nil, o.cfg.RequestTimeout); err != nil {
		session.mu.Lock()
		session.abortRequested = false
		session.mu.Unlock()
		return err
	}

	session.mu.Lock()
	if session.turnActive {
		session.finalizeTurnLocked(StateCancelled, nil, nowMS())
		session.emitSnapshotLocked()
	}
	session.mu.Unlock()
	return nil
}

// Stop implements Runtime. It closes stdin, waits for the child to end, and
// reaps it. Calling Stop twice is a no-op; a stopped session stays queryable.
func (o *OMP) Stop(ctx context.Context, sessionID string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	session, err := o.session(sessionID)
	if err != nil {
		return err
	}
	session.mu.Lock()
	if session.closed {
		session.mu.Unlock()
		return nil
	}
	session.stopping = true
	session.mu.Unlock()

	waitErr := session.proc.terminate(o.cfg.ShutdownGrace)
	state := StateDone
	if waitErr != nil {
		state = StateCancelled
	}
	session.finish(state, nil)
	return nil
}

// Approve implements Runtime. omp RPC v1 carries approvals as extension UI
// dialogs, so `note` has no transport field; it is accepted for contract
// compatibility and not forwarded.
func (o *OMP) Approve(ctx context.Context, sessionID, approvalID string, decision Decision, note string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if !decision.Valid() {
		return NewError(CodeInvalidArgument, "unknown approval decision %q", decision)
	}
	if approvalID == "" {
		return NewError(CodeInvalidArgument, "approval id is empty")
	}
	session, err := o.session(sessionID)
	if err != nil {
		return err
	}

	session.mu.Lock()
	entry, ok := session.uiPending[approvalID]
	if !ok {
		settled := session.approvalSettledLocked(approvalID)
		session.mu.Unlock()
		if settled {
			return NewError(CodeApprovalSettled, "approval %q was already settled", approvalID)
		}
		return NewError(CodeApprovalNotFound, "approval %q is not pending", approvalID)
	}
	session.mu.Unlock()

	response, err := approvalAnswer(entry.method, entry.options, decision)
	if err != nil {
		return err
	}
	response.ID = approvalID
	if err := session.proc.sendFrame(response); err != nil {
		session.proc.fail(err)
		return err
	}

	session.mu.Lock()
	defer session.mu.Unlock()
	current, ok := session.uiPending[approvalID]
	if !ok || current != entry {
		return NewError(CodeApprovalSettled, "approval %q was already settled", approvalID)
	}
	delete(session.uiPending, approvalID)
	if entry.timer != nil {
		entry.timer.Stop()
	}
	state := ApprovalApproved
	switch decision {
	case DecisionApprove:
		state = ApprovalApproved
	case DecisionDeny:
		state = ApprovalDenied
	default:
		state = ApprovalCancelled
	}
	settled, found := session.settleApprovalLocked(approvalID, state)
	if !found {
		settled = entry.approval
		settled.State = state
	}
	if session.currentTurn != "" {
		session.setTurnStateLocked(session.currentTurn, StateRunning)
	}
	if !session.closed {
		if session.turnActive {
			session.state = StateRunning
		} else {
			session.state = StateIdle
		}
	}
	session.emitApprovalLocked(settled)
	session.emitSnapshotLocked()
	return nil
}

// Resume implements Runtime. omp 18.18.1 exposes session files but the adapter
// cannot verify that a resumed process reproduces the transcript or approvals,
// so resume is explicitly unsupported rather than approximated.
func (o *OMP) Resume(ctx context.Context, sessionID, providerSessionID string) (Snapshot, error) {
	if err := ctx.Err(); err != nil {
		return Snapshot{}, err
	}
	return Snapshot{}, NewError(CodeUnsupported, "the omp adapter cannot verify resume; Capabilities.Resume is false")
}

// Events implements Runtime.
func (o *OMP) Events(sessionID string) <-chan Event {
	session, err := o.session(sessionID)
	if err != nil {
		closed := make(chan Event)
		close(closed)
		return closed
	}
	return session.events()
}

// List implements Runtime.
func (o *OMP) List() []Snapshot {
	o.mu.Lock()
	ids := make([]string, 0, len(o.sessions))
	sessions := make(map[string]*ompSession, len(o.sessions))
	for id, session := range o.sessions {
		ids = append(ids, id)
		sessions[id] = session
	}
	o.mu.Unlock()

	sort.Strings(ids)
	snapshots := make([]Snapshot, 0, len(ids))
	for _, id := range ids {
		snapshots = append(snapshots, sessions[id].snapshot())
	}
	return snapshots
}

// Close implements Runtime. Every child is stopped and reaped.
func (o *OMP) Close() error {
	o.closeOnce.Do(func() {
		o.mu.Lock()
		o.closed = true
		sessions := make([]*ompSession, 0, len(o.sessions))
		for _, session := range o.sessions {
			sessions = append(sessions, session)
		}
		o.mu.Unlock()

		for _, session := range sessions {
			if err := o.Stop(context.Background(), session.id); err != nil && o.closeErr == nil {
				o.closeErr = err
			}
		}
	})
	return o.closeErr
}

func (o *OMP) session(sessionID string) (*ompSession, error) {
	o.mu.Lock()
	defer o.mu.Unlock()
	session, ok := o.sessions[sessionID]
	if !ok {
		return nil, NewError(CodeSessionNotFound, "session %q is not known", sessionID)
	}
	return session, nil
}

func (o *OMP) liveLocked() int {
	live := 0
	for _, session := range o.sessions {
		if session.live() {
			live++
		}
	}
	return live
}

// ompSession owns the contract state of one omp child.
type ompSession struct {
	id   string
	cfg  Config
	proc *ompProcess

	readyCh   chan struct{}
	readyOnce sync.Once
	readyErr  error
	readyOK   atomic.Bool

	mu       sync.Mutex
	closed   bool
	stopping bool
	doneCh   chan struct{}

	state             State
	providerSessionID string
	turns             []Turn
	messages          []Message
	tools             []ToolCall
	approvals         []Approval
	settledApprovals  []Approval
	usage             Usage
	err               *Error
	updatedMS         int64

	turnActive     bool
	currentTurn    string
	currentMessage string
	abortRequested bool
	turnErr        *Error
	turnSeq        uint64
	messageSeq     uint64
	toolSeq        uint64
	approvalSeq    uint64
	unknownTypes   map[string]struct{}
	nextRequest    atomic.Uint64

	pending   map[string]chan responseFrame
	uiPending map[string]*pendingApproval

	history []Event
	seq     uint64
	subs    map[uint64]*subscriber
	nextSub uint64

	stderr []string
}

type pendingApproval struct {
	approval Approval
	method   string
	options  []string
	timer    *time.Timer
}

type subscriber struct {
	id uint64
	ch chan Event
}

func newOMPSession(cfg Config, req StartRequest) *ompSession {
	return &ompSession{
		id:           req.SessionID,
		cfg:          cfg,
		readyCh:      make(chan struct{}),
		doneCh:       make(chan struct{}),
		state:        StateUnknown,
		updatedMS:    nowMS(),
		pending:      make(map[string]chan responseFrame),
		uiPending:    make(map[string]*pendingApproval),
		subs:         make(map[uint64]*subscriber),
		unknownTypes: make(map[string]struct{}),
		stderr:       make([]string, 0, cfg.MaxStderrLines),
	}
}

func nowMS() int64 { return time.Now().UnixMilli() }

func (s *ompSession) markReady() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	s.state = StateIdle
	s.updatedMS = nowMS()
	s.emitSnapshotLocked()
}

func (s *ompSession) finishReady(err error) {
	s.readyOnce.Do(func() {
		s.readyErr = err
		if err == nil {
			s.readyOK.Store(true)
		}
		close(s.readyCh)
	})
}

func (s *ompSession) readyComplete() bool { return s.readyOK.Load() }

func (s *ompSession) live() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return !s.closed && s.proc != nil && !s.proc.exited()
}

// handleLine is called by the stdout reader for every frame. A returned error
// is a transport/protocol failure and stops the child.
func (s *ompSession) handleLine(line []byte) error {
	var probe struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(line, &probe); err != nil {
		return NewError(CodeProtocol, "malformed omp frame: %s", boundedMessage(err.Error(), 256))
	}
	if !s.readyComplete() {
		if probe.Type != "ready" {
			err := NewError(CodeProtocol, "expected an omp ready frame, got %q", probe.Type)
			s.finishReady(err)
			return err
		}
		info, err := parseReady(line)
		if err != nil {
			s.finishReady(err)
			return err
		}
		s.proc.setFrameCap(info.maxFrameBytes)
		s.mu.Lock()
		if !s.closed {
			s.state = StateIdle
			s.updatedMS = nowMS()
		}
		s.mu.Unlock()
		s.finishReady(nil)
		return nil
	}

	switch probe.Type {
	case "response":
		return s.handleResponse(line)
	case "extension_ui_request":
		return s.handleUI(line)
	case "host_tool_call":
		return s.refuseHostTool(line)
	case "host_uri_request":
		return s.refuseHostURI(line)
	case "rpc_chunk":
		s.emitDiagnostic(CodeUnsupported, "omp sent a protocol v2 chunk after negotiating v1")
		return nil
	case "":
		return NewError(CodeProtocol, "omp frame has no type")
	default:
		return s.handleEvent(probe.Type, line)
	}
}

func (s *ompSession) handleResponse(line []byte) error {
	var frame responseFrame
	if err := json.Unmarshal(line, &frame); err != nil {
		return NewError(CodeProtocol, "malformed omp response: %s", boundedMessage(err.Error(), 256))
	}
	if frame.Success == nil {
		return NewError(CodeProtocol, "omp response for %q is missing success", frame.Command)
	}

	s.mu.Lock()
	waiter := s.pending[frame.ID]
	if waiter != nil {
		delete(s.pending, frame.ID)
	}
	s.mu.Unlock()

	if waiter != nil {
		select {
		case waiter <- frame:
		default:
		}
		return nil
	}
	if !*frame.Success {
		s.emitDiagnostic(CodeRejected, "uncorrelated omp failure for %q: %s", frame.Command, boundedMessage(frame.Error, 512))
	}
	return nil
}

// request sends one command and waits for its correlated acknowledgement.
// A timeout removes the correlation entry but does not kill a healthy child.
func (s *ompSession) request(ctx context.Context, command string, payload map[string]any, timeout time.Duration) (responseFrame, error) {
	if err := s.notRunningError(); err != nil {
		return responseFrame{}, err
	}
	id := fmt.Sprintf("ukk-%d", s.nextRequest.Add(1))
	waiter := make(chan responseFrame, 1)

	s.mu.Lock()
	if s.closed {
		err := s.exitErrorLocked()
		s.mu.Unlock()
		return responseFrame{}, err
	}
	s.pending[id] = waiter
	s.mu.Unlock()

	frame := map[string]any{"type": command, "id": id}
	for key, value := range payload {
		frame[key] = value
	}
	if err := s.proc.sendFrame(frame); err != nil {
		s.removePending(id)
		s.proc.fail(err)
		return responseFrame{}, err
	}

	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case response := <-waiter:
		if response.Success == nil || !*response.Success {
			message := response.Error
			if message == "" {
				message = "omp rejected the command"
			}
			suffix := ""
			if response.Code != "" {
				suffix = " (" + boundedMessage(response.Code, 64) + ")"
			}
			return response, NewError(CodeRejected, "omp rejected %q: %s%s", command, boundedMessage(message, 512), suffix)
		}
		return response, nil
	case <-ctx.Done():
		s.removePending(id)
		return responseFrame{}, ctx.Err()
	case <-timer.C:
		s.removePending(id)
		return responseFrame{}, NewError(CodeTimeout, "omp did not acknowledge %q within %s", command, timeout)
	case <-s.doneCh:
		s.removePending(id)
		return responseFrame{}, s.exitError()
	}
}

func (s *ompSession) removePending(id string) {
	s.mu.Lock()
	delete(s.pending, id)
	s.mu.Unlock()
}

func (s *ompSession) notRunningError() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return s.exitErrorLocked()
	}
	if s.proc == nil || s.proc.exited() {
		return NewError(CodeNotRunning, "omp session %q is not running", s.id)
	}
	return nil
}

func (s *ompSession) exitError() *Error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.exitErrorLocked()
}

func (s *ompSession) exitErrorLocked() *Error {
	if s.err != nil {
		copied := *s.err
		return &copied
	}
	return NewError(CodeNotRunning, "omp session %q is not running", s.id)
}

func (s *ompSession) beginPrompt(now int64, text string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return "", s.exitErrorLocked()
	}
	if s.proc == nil || s.proc.exited() {
		return "", NewError(CodeNotRunning, "omp session %q is not running", s.id)
	}
	if s.turnActive || len(s.uiPending) > 0 {
		return "", NewError(CodeBusy, "session %q already has a running turn", s.id)
	}
	s.turnSeq++
	turnID := fmt.Sprintf("turn-%d", s.turnSeq)
	s.turnActive = true
	s.currentTurn = turnID
	s.turnErr = nil
	s.abortRequested = false
	s.turns = appendBounded(s.turns, Turn{ID: turnID, State: StateRunning, StartedMS: now}, maxTurns)

	s.messageSeq++
	message := Message{
		ID:        fmt.Sprintf("user-%d", s.messageSeq),
		Role:      RoleUser,
		Text:      text,
		Final:     true,
		CreatedMS: now,
	}
	s.messages = appendBounded(s.messages, message, maxMessages)
	s.state = StateRunning
	s.updatedMS = now
	s.emitLocked(EventMessage, func(event *Event) { event.Message = &message })
	s.emitSnapshotLocked()
	return turnID, nil
}

func (s *ompSession) failTurn(turnID string, cause error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.currentTurn != turnID {
		return
	}
	state := StateFailed
	if errors.Is(cause, context.Canceled) || errors.Is(cause, context.DeadlineExceeded) {
		state = StateCancelled
	}
	turnErr := errorOf(cause)
	s.finalizeTurnLocked(state, turnErr, nowMS())
	if turnErr != nil {
		failure := *turnErr
		s.emitLocked(EventError, func(event *Event) { event.Error = &failure })
	}
	s.emitSnapshotLocked()
}

func errorOf(err error) *Error {
	var typed *Error
	if errors.As(err, &typed) {
		copied := *typed
		return &copied
	}
	if err == nil {
		return nil
	}
	return NewError(CodeTurnFailed, "%s", err.Error())
}

func (s *ompSession) handleEvent(kind string, line []byte) error {
	var frame rpcEvent
	if err := json.Unmarshal(line, &frame); err != nil {
		return NewError(CodeProtocol, "malformed omp %s frame: %s", kind, boundedMessage(err.Error(), 256))
	}
	now := nowMS()
	switch kind {
	case "agent_start", "turn_start":
		s.startTurn(now)
	case "turn_end":
		// The documented terminal event is agent_end; turn_end only closes the
		// structured turn. Keeping the run active avoids admitting a second
		// prompt before the agent is actually done.
		s.closeTurnAt(now)
	case "message_start":
		s.beginMessage(&frame, now)
	case "message_update":
		s.updateMessage(&frame, now)
	case "message_end":
		s.endMessage(&frame, now)
	case "tool_execution_start":
		s.startTool(&frame, now)
	case "tool_execution_update", "tool_stream_update":
		s.updateTool(&frame, now)
	case "tool_execution_end":
		s.endTool(&frame, now)
	case "agent_end":
		s.handleAgentEnd(&frame, now)
	case "prompt_result":
		s.handlePromptResult(&frame, now)
	case "session_info_update":
		s.handleSessionInfo(&frame, now)
	case "notice":
		s.handleNotice(line)
	case "command_output":
		s.handleCommandOutput(&frame, now)
	case "extension_error":
		s.emitDiagnostic(CodeDiagnostic, "omp extension %q failed handling %q: %s", frame.ExtensionPath, frame.Event, boundedMessage(rawText(frame.Error), 512))
	case "rpc_frame_error":
		s.emitDiagnostic(CodeProtocol, "omp dropped a frame that exceeded its transport limits: %s", boundedMessage(rawText(frame.Error), 512))
	case "session_settled":
		s.handleSettled(now)
	default:
		notify := false
		s.mu.Lock()
		if _, seen := s.unknownTypes[kind]; !seen && len(s.unknownTypes) < maxUnknownEventTypes {
			s.unknownTypes[kind] = struct{}{}
			notify = true
		}
		s.mu.Unlock()
		if notify {
			s.emitDiagnostic(CodeDiagnostic, "ignored unknown omp event type %q", kind)
		}
	}
	return nil
}

func (s *ompSession) startTurn(now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	if !s.turnActive || s.currentTurn == "" {
		s.turnSeq++
		s.currentTurn = fmt.Sprintf("turn-%d", s.turnSeq)
		s.turnActive = true
		s.turns = appendBounded(s.turns, Turn{ID: s.currentTurn, State: StateRunning, StartedMS: now}, maxTurns)
	} else {
		s.setTurnStateLocked(s.currentTurn, StateRunning)
	}
	s.state = StateRunning
	s.updatedMS = now
	s.emitSnapshotLocked()
}

func (s *ompSession) closeTurnAt(now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.currentTurn == "" {
		return
	}
	for index := range s.turns {
		if s.turns[index].ID == s.currentTurn && s.turns[index].EndedMS == 0 {
			s.turns[index].EndedMS = now
		}
	}
}

func (s *ompSession) beginMessage(frame *rpcEvent, now int64) {
	message, ok := decodeAgentMessage(frame.Message)
	if !ok || message.Role != "assistant" {
		return
	}
	created := now
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	id := frame.MessageID
	if id == "" {
		s.messageSeq++
		id = fmt.Sprintf("local-%d", s.messageSeq)
	}
	text := messageText(message.Content)
	record := s.upsertMessageLocked(id, RoleAssistant, text, false, created)
	s.currentMessage = id
	s.emitLocked(EventMessage, func(event *Event) { event.Message = &record })
	s.emitSnapshotLocked()
}

func (s *ompSession) updateMessage(frame *rpcEvent, now int64) {
	update := frame.AssistantMessageEvent
	if update == nil {
		return
	}
	switch update.Type {
	case "text_delta":
		s.mu.Lock()
		defer s.mu.Unlock()
		if s.closed {
			return
		}
		id := s.ensureAssistantMessageLocked(frame.MessageID, now)
		record := s.appendMessageTextLocked(id, update.Delta)
		s.emitLocked(EventMessage, func(event *Event) { event.Message = &record })
	case "text_end":
		if update.Content == "" {
			return
		}
		s.mu.Lock()
		defer s.mu.Unlock()
		if s.closed {
			return
		}
		id := s.ensureAssistantMessageLocked(frame.MessageID, now)
		record := s.replaceMessageTextLocked(id, update.Content)
		s.emitLocked(EventMessage, func(event *Event) { event.Message = &record })
	case "done":
		message, ok := decodeAgentMessage(update.Message)
		if !ok {
			return
		}
		s.mu.Lock()
		defer s.mu.Unlock()
		if s.closed {
			return
		}
		id := s.ensureAssistantMessageLocked(frame.MessageID, now)
		record := s.replaceMessageTextLocked(id, messageText(message.Content))
		if usage, reported := usageFromFrame(message.Usage); reported {
			s.usage = usage
			s.emitLocked(EventUsage, func(event *Event) { event.Usage = &usage })
		}
		s.emitLocked(EventMessage, func(event *Event) { event.Message = &record })
	case "error":
		message, ok := decodeAgentMessage(update.Error)
		if !ok || message.ErrorMessage == "" {
			return
		}
		s.mu.Lock()
		s.turnErr = NewError(CodeTurnFailed, "%s", boundedMessage(message.ErrorMessage, 1024))
		s.mu.Unlock()
		s.emitDiagnostic(CodeTurnFailed, "omp turn failed: %s", message.ErrorMessage)
	case "toolcall_end":
		s.registerToolCall(update.ToolCall, now)
	}
}

func (s *ompSession) endMessage(frame *rpcEvent, now int64) {
	message, ok := decodeAgentMessage(frame.Message)
	if !ok {
		return
	}
	if message.Role != "assistant" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	id := frame.MessageID
	if id == "" {
		id = s.currentMessage
	}
	if id == "" {
		s.messageSeq++
		id = fmt.Sprintf("local-%d", s.messageSeq)
	}
	text := messageText(message.Content)
	record := s.upsertMessageLocked(id, RoleAssistant, text, true, now)
	s.currentMessage = ""
	var usageEvent *Usage
	if usage, reported := usageFromFrame(message.Usage); reported {
		s.usage = usage
		usageEvent = &usage
	}
	if message.ErrorMessage != "" {
		s.turnErr = NewError(CodeTurnFailed, "%s", boundedMessage(message.ErrorMessage, 1024))
	}
	s.emitLocked(EventMessage, func(event *Event) { event.Message = &record })
	if usageEvent != nil {
		s.emitLocked(EventUsage, func(event *Event) { event.Usage = usageEvent })
	}
	s.emitSnapshotLocked()
}

func (s *ompSession) startTool(frame *rpcEvent, now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	id := frame.ToolCallID
	if id == "" {
		s.toolSeq++
		id = fmt.Sprintf("tool-%d", s.toolSeq)
	}
	record := s.upsertToolLocked(id, frame.ToolName, frame.Args, ToolRunning, now)
	record.TurnID = s.currentTurn
	s.putToolLocked(record)
	s.emitLocked(EventTool, func(event *Event) { event.Tool = &record })
	s.emitSnapshotLocked()
}

func (s *ompSession) updateTool(frame *rpcEvent, now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	id := frame.ToolCallID
	if id == "" {
		return
	}
	record, ok := s.findToolLocked(id)
	if !ok {
		record = s.upsertToolLocked(id, frame.ToolName, nil, ToolRunning, now)
		record.TurnID = s.currentTurn
	}
	if frame.ToolName != "" {
		record.Name = frame.ToolName
	}
	record.Status = ToolRunning
	record.Output = cloneRaw(frame.PartialResult)
	s.putToolLocked(record)
	s.emitLocked(EventTool, func(event *Event) { event.Tool = &record })
}

func (s *ompSession) endTool(frame *rpcEvent, now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	id := frame.ToolCallID
	if id == "" {
		return
	}
	record, ok := s.findToolLocked(id)
	if !ok {
		record = s.upsertToolLocked(id, frame.ToolName, nil, ToolRunning, now)
		record.TurnID = s.currentTurn
	}
	if frame.ToolName != "" {
		record.Name = frame.ToolName
	}
	record.Output = cloneRaw(frame.Result)
	record.Status = ToolCompleted
	record.IsError = frame.IsError != nil && *frame.IsError
	if record.IsError {
		record.Status = ToolFailed
	}
	record.EndedMS = now
	s.putToolLocked(record)
	s.emitLocked(EventTool, func(event *Event) { event.Tool = &record })
	s.emitSnapshotLocked()
}

func (s *ompSession) registerToolCall(raw json.RawMessage, now int64) {
	if len(raw) == 0 {
		return
	}
	var call struct {
		ID        string          `json:"id"`
		Name      string          `json:"name"`
		Arguments json.RawMessage `json:"arguments"`
	}
	if err := json.Unmarshal(raw, &call); err != nil || call.ID == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	if _, ok := s.findToolLocked(call.ID); ok {
		return
	}
	record := s.upsertToolLocked(call.ID, call.Name, call.Arguments, ToolPending, now)
	record.TurnID = s.currentTurn
	s.putToolLocked(record)
	s.emitLocked(EventTool, func(event *Event) { event.Tool = &record })
}

func (s *ompSession) handleAgentEnd(frame *rpcEvent, now int64) {
	if frame.IsTerminal != nil && !*frame.IsTerminal {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	state := StateDone
	var turnErr *Error
	if s.turnErr != nil {
		state = StateFailed
		turnErr = s.turnErr
	}
	if s.abortRequested {
		state = StateCancelled
	}
	s.finalizeTurnLocked(state, turnErr, now)
	if turnErr != nil {
		failure := *turnErr
		s.emitLocked(EventError, func(event *Event) { event.Error = &failure })
	}
	s.emitSnapshotLocked()
}

func (s *ompSession) handlePromptResult(frame *rpcEvent, now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	state := StateDone
	var turnErr *Error
	switch frame.Status {
	case "aborted":
		state = StateCancelled
	case "error":
		state = StateFailed
		turnErr = promptErrorFrom(frame)
	case "completed", "":
		state = StateDone
	default:
		// An undocumented status is reported as unknown, not guessed.
		state = StateUnknown
	}
	if s.abortRequested {
		state = StateCancelled
		turnErr = nil
	}
	if turnErr != nil {
		s.turnErr = turnErr
	}
	s.finalizeTurnLocked(state, turnErr, now)
	if turnErr != nil {
		failure := *turnErr
		s.emitLocked(EventError, func(event *Event) { event.Error = &failure })
	}
	s.emitSnapshotLocked()
}

func promptErrorFrom(frame *rpcEvent) *Error {
	var detail promptError
	if len(frame.Error) > 0 {
		_ = json.Unmarshal(frame.Error, &detail)
	}
	message := detail.Message
	if message == "" {
		message = "omp reported a failed turn"
	}
	return NewError(CodeTurnFailed, "%s", boundedMessage(message, 1024))
}

func (s *ompSession) handleSessionInfo(frame *rpcEvent, now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	if frame.SessionID != "" {
		s.providerSessionID = frame.SessionID
	}
	s.updatedMS = now
	s.emitSnapshotLocked()
}

func (s *ompSession) handleNotice(line []byte) {
	var frame noticeFrame
	if err := json.Unmarshal(line, &frame); err != nil {
		return
	}
	if strings.EqualFold(frame.Level, "error") {
		s.emitDiagnostic(CodeDiagnostic, "omp notice: %s", boundedMessage(frame.Message, 512))
	}
}

func (s *ompSession) handleCommandOutput(frame *rpcEvent, now int64) {
	text := strings.TrimSpace(frame.Text)
	if text == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	s.messageSeq++
	record := Message{
		ID:        fmt.Sprintf("sys-%d", s.messageSeq),
		Role:      RoleSystem,
		Text:      text,
		Final:     true,
		CreatedMS: now,
	}
	s.messages = appendBounded(s.messages, record, maxMessages)
	s.emitLocked(EventMessage, func(event *Event) { event.Message = &record })
	s.emitSnapshotLocked()
}

func (s *ompSession) handleSettled(now int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed || s.turnActive {
		return
	}
	s.state = StateIdle
	s.updatedMS = now
	s.emitSnapshotLocked()
}

// finalizeTurnLocked settles the current turn exactly once. Later terminal
// frames (agent_end after prompt_result or vice versa) are no-ops.
func (s *ompSession) finalizeTurnLocked(state State, turnErr *Error, now int64) {
	if s.currentTurn != "" {
		s.setTurnStateLocked(s.currentTurn, state)
		for index := range s.turns {
			if s.turns[index].ID == s.currentTurn {
				if s.turns[index].EndedMS == 0 {
					s.turns[index].EndedMS = now
				}
				s.turns[index].Error = turnErr
				break
			}
		}
	}
	s.turnActive = false
	s.currentTurn = ""
	s.currentMessage = ""
	s.abortRequested = false
	s.turnErr = nil
	for id, pending := range s.uiPending {
		if pending.timer != nil {
			pending.timer.Stop()
		}
		delete(s.uiPending, id)
		settled, found := s.settleApprovalLocked(id, ApprovalCancelled)
		if !found {
			settled = pending.approval
			settled.State = ApprovalCancelled
		}
		s.emitApprovalLocked(settled)
	}
	if !s.closed {
		s.state = StateIdle
	}
	s.updatedMS = now
}

func (s *ompSession) setTurnStateLocked(turnID string, state State) {
	for index := range s.turns {
		if s.turns[index].ID == turnID {
			s.turns[index].State = state
			return
		}
	}
}

func (s *ompSession) upsertMessageLocked(id string, role Role, text string, final bool, createdMS int64) Message {
	for index := range s.messages {
		if s.messages[index].ID != id {
			continue
		}
		if text != "" {
			s.messages[index].Text = text
		}
		s.messages[index].Final = final
		if role != "" {
			s.messages[index].Role = role
		}
		return s.messages[index]
	}
	record := Message{ID: id, Role: role, Text: text, Final: final, CreatedMS: createdMS}
	s.messages = appendBounded(s.messages, record, maxMessages)
	return record
}

func (s *ompSession) ensureAssistantMessageLocked(id string, now int64) string {
	if id == "" {
		id = s.currentMessage
	}
	if id == "" {
		s.messageSeq++
		id = fmt.Sprintf("local-%d", s.messageSeq)
	}
	s.upsertMessageLocked(id, RoleAssistant, "", false, now)
	s.currentMessage = id
	return id
}

func (s *ompSession) appendMessageTextLocked(id, delta string) Message {
	for index := range s.messages {
		if s.messages[index].ID == id {
			s.messages[index].Text += delta
			return s.messages[index]
		}
	}
	record := Message{ID: id, Role: RoleAssistant, Text: delta, CreatedMS: nowMS()}
	s.messages = appendBounded(s.messages, record, maxMessages)
	return record
}

func (s *ompSession) replaceMessageTextLocked(id, text string) Message {
	for index := range s.messages {
		if s.messages[index].ID == id {
			s.messages[index].Text = text
			return s.messages[index]
		}
	}
	record := Message{ID: id, Role: RoleAssistant, Text: text, CreatedMS: nowMS()}
	s.messages = appendBounded(s.messages, record, maxMessages)
	return record
}

func (s *ompSession) upsertToolLocked(id, name string, input json.RawMessage, status ToolStatus, now int64) ToolCall {
	for index := range s.tools {
		if s.tools[index].ID != id {
			continue
		}
		if name != "" {
			s.tools[index].Name = name
		}
		if len(input) > 0 {
			s.tools[index].Input = cloneRaw(input)
		}
		s.tools[index].Status = status
		if s.tools[index].StartedMS == 0 {
			s.tools[index].StartedMS = now
		}
		return s.tools[index]
	}
	record := ToolCall{
		ID:        id,
		Name:      name,
		Input:     cloneRaw(input),
		Status:    status,
		StartedMS: now,
	}
	s.tools = appendBounded(s.tools, record, maxTools)
	return record
}

func (s *ompSession) findToolLocked(id string) (ToolCall, bool) {
	for index := range s.tools {
		if s.tools[index].ID == id {
			return s.tools[index], true
		}
	}
	return ToolCall{}, false
}

// putToolLocked stores a full tool record, replacing the previous value.
func (s *ompSession) putToolLocked(record ToolCall) {
	for index := range s.tools {
		if s.tools[index].ID == record.ID {
			s.tools[index] = record
			return
		}
	}
	s.tools = appendBounded(s.tools, record, maxTools)
}

// handleUI interprets extension UI requests. Tool approvals are the only
// dialogs the contract exposes; every other value-expecting dialog is cancelled
// so the extension never deadlocks, and fire-and-forget notifications are
// ignored.
func (s *ompSession) handleUI(line []byte) error {
	var frame uiRequestFrame
	if err := json.Unmarshal(line, &frame); err != nil {
		return NewError(CodeProtocol, "malformed omp extension_ui_request: %s", boundedMessage(err.Error(), 256))
	}
	now := nowMS()
	switch frame.Method {
	case "select", "confirm", "input", "editor", "ask":
		approval, ok := approvalFromUI(&frame, now)
		if !ok {
			if err := s.proc.sendFrame(uiResponseFrame{Type: "extension_ui_response", ID: frame.ID, Cancelled: new(true)}); err != nil {
				return err
			}
			s.emitDiagnostic(CodeUIUnsupported, "auto-cancelled omp UI request %q", frame.Method)
			return nil
		}
		if !s.registerApproval(approval, frame.Method, frame.Options, frame.TimeoutMS, now) {
			if err := s.proc.sendFrame(uiResponseFrame{Type: "extension_ui_response", ID: frame.ID, Cancelled: new(true)}); err != nil {
				return err
			}
			s.emitDiagnostic(CodeLimitExceeded, "cancelled an omp approval beyond the %d-pending limit", maxApprovals)
		}
		return nil
	case "cancel":
		s.cancelApproval(frame.TargetID, now)
		return nil
	default:
		// notify, setStatus, setWidget, setTitle, set_editor_text, open_url and
		// other fire-and-forget requests need no response.
		return nil
	}
}

func (s *ompSession) registerApproval(approval Approval, method string, options []string, timeoutMS int64, now int64) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed || len(s.uiPending) >= maxApprovals {
		return false
	}
	s.approvalSeq++
	if approval.ID == "" {
		approval.ID = fmt.Sprintf("approval-%d", s.approvalSeq)
	}
	approval.TurnID = s.currentTurn
	entry := &pendingApproval{
		approval: approval,
		method:   method,
		options:  append([]string(nil), options...),
	}
	s.uiPending[approval.ID] = entry
	s.approvals = appendBounded(s.approvals, approval, maxApprovals)
	if s.currentTurn != "" {
		s.setTurnStateLocked(s.currentTurn, StateBlocked)
	}
	s.state = StateBlocked
	s.updatedMS = now
	if timeoutMS > 0 {
		id := approval.ID
		entry.timer = time.AfterFunc(time.Duration(timeoutMS)*time.Millisecond, func() {
			s.expireApproval(id)
		})
	}
	s.emitApprovalLocked(approval)
	s.emitSnapshotLocked()
	return true
}

func (s *ompSession) expireApproval(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	entry, ok := s.uiPending[id]
	if !ok || s.closed {
		return
	}
	delete(s.uiPending, id)
	if entry.timer != nil {
		entry.timer.Stop()
	}
	settled, found := s.settleApprovalLocked(id, ApprovalExpired)
	if !found {
		settled = entry.approval
		settled.State = ApprovalExpired
	}
	s.resumeAfterApprovalLocked()
	s.emitApprovalLocked(settled)
	s.emitSnapshotLocked()
}

func (s *ompSession) cancelApproval(id string, now int64) {
	if id == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	entry, ok := s.uiPending[id]
	if !ok || s.closed {
		return
	}
	delete(s.uiPending, id)
	if entry.timer != nil {
		entry.timer.Stop()
	}
	settled, found := s.settleApprovalLocked(id, ApprovalCancelled)
	if !found {
		settled = entry.approval
		settled.State = ApprovalCancelled
	}
	s.resumeAfterApprovalLocked()
	s.updatedMS = now
	s.emitApprovalLocked(settled)
	s.emitSnapshotLocked()
}

func (s *ompSession) resumeAfterApprovalLocked() {
	if s.currentTurn != "" {
		s.setTurnStateLocked(s.currentTurn, StateRunning)
	}
	if !s.closed {
		if s.turnActive {
			s.state = StateRunning
		} else {
			s.state = StateIdle
		}
	}
	s.updatedMS = nowMS()
}

func (s *ompSession) settleApprovalLocked(id string, state ApprovalState) (Approval, bool) {
	for index := range s.approvals {
		if s.approvals[index].ID != id {
			continue
		}
		s.approvals[index].State = state
		settled := s.approvals[index]
		s.approvals = append(s.approvals[:index], s.approvals[index+1:]...)
		s.settledApprovals = appendBounded(s.settledApprovals, settled, maxApprovals)
		return settled, true
	}
	return Approval{}, false
}

func (s *ompSession) approvalSettledLocked(id string) bool {
	for index := range s.settledApprovals {
		if s.settledApprovals[index].ID == id {
			return s.settledApprovals[index].State != ApprovalPending
		}
	}
	return false
}

func (s *ompSession) refuseHostTool(line []byte) error {
	var frame struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(line, &frame); err != nil {
		return NewError(CodeProtocol, "malformed omp host_tool_call: %s", boundedMessage(err.Error(), 256))
	}
	if frame.ID != "" {
		if err := s.proc.sendFrame(map[string]any{
			"type":    "host_tool_result",
			"id":      frame.ID,
			"isError": true,
			"result": map[string]any{
				"content": []any{map[string]any{"type": "text", "text": "ukk Desktop does not register omp host tools"}},
				"isError": true,
			},
		}); err != nil {
			return err
		}
	}
	s.emitDiagnostic(CodeUnsupported, "refused an omp host tool call; this Host registers no host tools")
	return nil
}

func (s *ompSession) refuseHostURI(line []byte) error {
	var frame struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(line, &frame); err != nil {
		return NewError(CodeProtocol, "malformed omp host_uri_request: %s", boundedMessage(err.Error(), 256))
	}
	if frame.ID != "" {
		if err := s.proc.sendFrame(map[string]any{
			"type":    "host_uri_result",
			"id":      frame.ID,
			"isError": true,
			"error":   "ukk Desktop does not register omp host URI schemes",
		}); err != nil {
			return err
		}
	}
	s.emitDiagnostic(CodeUnsupported, "refused an omp host URI request; this Host registers no schemes")
	return nil
}

func (s *ompSession) fetchState() {
	timeout := min(stateQueryTimeout, s.cfg.RequestTimeout)
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	response, err := s.request(ctx, "get_state", nil, timeout)
	if err != nil {
		return
	}
	state, err := decodeSessionState(response.Data)
	if err != nil || state.SessionID == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	s.providerSessionID = state.SessionID
	s.updatedMS = nowMS()
	s.emitSnapshotLocked()
}

// fail settles a broken child. Explicit Stop and Abort paths are excluded so a
// clean shutdown is not reported as a failure.
func (s *ompSession) fail(cause error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	stopping := s.stopping
	waitErr := s.proc.exitErr()
	stderr := s.stderrTailLocked()
	s.mu.Unlock()

	if stopping {
		if waitErr != nil {
			s.finish(StateCancelled, nil)
		} else {
			s.finish(StateDone, nil)
		}
		return
	}
	code := CodeTransport
	var typed *Error
	if errors.As(cause, &typed) {
		code = typed.Code
	}
	if waitErr != nil && code != CodeProtocol && code != CodeInvalidArgument {
		code = CodeChildExited
	}
	detail := describeExit(waitErr)
	message := detail
	if cause != nil {
		message = boundedMessage(cause.Error(), 1024) + "; " + detail
	}
	if len(stderr) > 0 {
		message += " (stderr: " + strings.Join(stderr, " | ") + ")"
	}
	s.finish(StateFailed, NewError(code, "%s", message))
}

// finish settles a session exactly once: it closes the active turn, cancels
// pending approvals, emits the final snapshot and a closed event, and closes
// every subscriber channel.
func (s *ompSession) finish(state State, sessionErr *Error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	now := nowMS()
	turnState := state
	if turnState == StateIdle || turnState == StateUnknown {
		turnState = StateFailed
	}
	s.finalizeTurnLocked(turnState, sessionErr, now)
	s.state = state
	s.err = sessionErr
	s.updatedMS = now
	if sessionErr != nil {
		failure := *sessionErr
		s.emitLocked(EventError, func(event *Event) { event.Error = &failure })
	}
	s.emitSnapshotLocked()
	s.emitLocked(EventClosed, nil)
	s.closed = true
	close(s.doneCh)
	for _, sub := range s.subs {
		close(sub.ch)
	}
	s.subs = make(map[uint64]*subscriber)
	s.finishReady(sessionErr)
}

// snapshot returns a deep copy of the renderable state.
func (s *ompSession) snapshot() Snapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.snapshotLocked()
}

func (s *ompSession) snapshotLocked() Snapshot {
	turns := make([]Turn, len(s.turns))
	for index, turn := range s.turns {
		if turn.Error != nil {
			copied := *turn.Error
			turn.Error = &copied
		}
		turns[index] = turn
	}
	tools := make([]ToolCall, len(s.tools))
	for index, tool := range s.tools {
		tool.Input = cloneRaw(tool.Input)
		tool.Output = cloneRaw(tool.Output)
		tools[index] = tool
	}
	approvals := make([]Approval, len(s.approvals))
	for index, approval := range s.approvals {
		approval.Input = cloneRaw(approval.Input)
		approvals[index] = approval
	}
	snapshot := Snapshot{
		ID:                s.id,
		Provider:          ProviderNameOMP,
		ProviderSessionID: s.providerSessionID,
		State:             s.state,
		Turns:             turns,
		Messages:          append(make([]Message, 0, len(s.messages)), s.messages...),
		Tools:             tools,
		PendingApprovals:  approvals,
		Usage:             s.usage,
		Provenance:        ProvenanceOMPRPC,
		UpdatedMS:         s.updatedMS,
	}
	if s.err != nil {
		copied := *s.err
		snapshot.Error = &copied
	}
	return snapshot
}

func (s *ompSession) events() <-chan Event {
	capacity := s.cfg.EventHistory + s.cfg.SubscriberQueue
	if capacity < 16 {
		capacity = 16
	}
	channel := make(chan Event, capacity)

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		replay := s.history
		if len(replay) > capacity {
			replay = replay[len(replay)-capacity:]
		}
		for _, event := range replay {
			channel <- event
		}
		close(channel)
		return channel
	}
	id := s.nextSub
	if len(s.subs) >= maxSubscribers {
		replay := s.history
		if len(replay) > capacity {
			replay = replay[len(replay)-capacity:]
		}
		for _, event := range replay {
			channel <- event
		}
		close(channel)
		return channel
	}
	s.nextSub++
	s.subs[id] = &subscriber{id: id, ch: channel}
	replay := s.history
	if len(replay) > capacity {
		replay = replay[len(replay)-capacity:]
	}
	for _, event := range replay {
		channel <- event
	}
	return channel
}

func (sub *subscriber) send(event Event) {
	select {
	case sub.ch <- event:
		return
	default:
	}
	// Drop the oldest queued event instead of blocking the stdout reader. The
	// snapshot remains authoritative for a subscriber that falls behind.
	select {
	case <-sub.ch:
	default:
	}
	select {
	case sub.ch <- event:
	default:
	}
}

func (s *ompSession) emitLocked(kind EventKind, fill func(*Event)) {
	if s.closed && kind != EventClosed {
		return
	}
	s.seq++
	event := Event{Kind: kind, SessionID: s.id, Sequence: s.seq}
	if fill != nil {
		fill(&event)
	}
	s.history = appendBounded(s.history, event, s.cfg.EventHistory)
	for _, sub := range s.subs {
		sub.send(event)
	}
}

func (s *ompSession) emitSnapshotLocked() {
	s.updatedMS = nowMS()
	snapshot := s.snapshotLocked()
	s.emitLocked(EventSnapshot, func(event *Event) { event.Snapshot = &snapshot })
}

func (s *ompSession) emitApprovalLocked(approval Approval) {
	record := approval
	s.emitLocked(EventApproval, func(event *Event) { event.Approval = &record })
}

func (s *ompSession) emitDiagnostic(code ErrorCode, format string, args ...any) {
	diagnostic := NewError(code, format, args...)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.emitLocked(EventDiagnostic, func(event *Event) { event.Error = diagnostic })
}

func (s *ompSession) onStderr(line string) {
	text := boundedLine(line, maxStderrLineBytes)
	if text == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.stderr = appendBounded(s.stderr, text, s.cfg.MaxStderrLines)
}

func (s *ompSession) stderrTail() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.stderrTailLocked()
}

func (s *ompSession) stderrTailLocked() []string {
	return append(make([]string, 0, len(s.stderr)), s.stderr...)
}

func appendBounded[T any](slice []T, value T, limit int) []T {
	if limit <= 0 {
		return append(slice, value)
	}
	slice = append(slice, value)
	if len(slice) > limit {
		slice = slice[len(slice)-limit:]
	}
	return slice
}

func cloneRaw(raw json.RawMessage) json.RawMessage {
	if len(raw) == 0 {
		return nil
	}
	return append(json.RawMessage(nil), raw...)
}

// startArgs appends the request's model and profile selectors to the configured
// base arguments. The base list always includes `--mode rpc`.
func startArgs(base []string, req StartRequest) []string {
	args := append([]string(nil), base...)
	if req.Model != "" {
		args = append(args, "--model", req.Model)
	}
	if req.Profile != "" {
		args = append(args, "--profile", req.Profile)
	}
	return args
}

func validateStartRequest(req StartRequest) error {
	id := strings.TrimSpace(req.SessionID)
	if id == "" {
		return NewError(CodeInvalidArgument, "session id is empty")
	}
	if len(id) > maxSessionIDLen {
		return NewError(CodeInvalidArgument, "session id exceeds %d bytes", maxSessionIDLen)
	}
	for _, r := range id {
		if r < 0x20 || r == 0x7f {
			return NewError(CodeInvalidArgument, "session id contains a control character")
		}
	}
	if len(req.Model) > 512 {
		return NewError(CodeInvalidArgument, "model name exceeds 512 bytes")
	}
	if len(req.Profile) > 512 {
		return NewError(CodeInvalidArgument, "profile name exceeds 512 bytes")
	}
	for _, selector := range []string{req.Model, req.Profile} {
		for _, r := range selector {
			if r < 0x20 || r == 0x7f {
				return NewError(CodeInvalidArgument, "model or profile contains a control character")
			}
		}
	}
	if req.Cwd != "" {
		if !filepath.IsAbs(req.Cwd) {
			return NewError(CodeInvalidArgument, "cwd must be an absolute path")
		}
		info, err := os.Stat(req.Cwd)
		if err != nil || !info.IsDir() {
			return NewError(CodeInvalidArgument, "cwd %q is not a directory", req.Cwd)
		}
	}
	total := 0
	for key, value := range req.Env {
		if key == "" || strings.ContainsAny(key, "=\x00") {
			return NewError(CodeInvalidArgument, "environment variable name %q is invalid", key)
		}
		if strings.ContainsRune(value, '\x00') {
			return NewError(CodeInvalidArgument, "environment variable %q contains a NUL byte", key)
		}
		total += len(key) + len(value)
	}
	if total > maxEnvBytes {
		return NewError(CodeInvalidArgument, "environment exceeds %d bytes", maxEnvBytes)
	}
	return nil
}
