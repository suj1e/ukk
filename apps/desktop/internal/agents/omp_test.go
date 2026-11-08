package agents

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

func testRuntime(t *testing.T, mutate func(*Config)) *OMP {
	t.Helper()
	cfg := Config{
		Binary:         os.Args[0],
		Args:           []string{"-test.run=TestHelperProcess", "--"},
		StartupTimeout: 10 * time.Second,
		RequestTimeout: 5 * time.Second,
		ShutdownGrace:  2 * time.Second,
	}
	if mutate != nil {
		mutate(&cfg)
	}
	runtime, err := NewOMP(cfg)
	if err != nil {
		t.Fatalf("NewOMP: %v", err)
	}
	t.Cleanup(func() { _ = runtime.Close() })
	return runtime
}

func startRequest(t *testing.T, id, scenario, pidFile string) StartRequest {
	t.Helper()
	env := map[string]string{helperEnv: "1", scenarioEnv: scenario}
	if pidFile != "" {
		env[helperPIDFile] = pidFile
	}
	return StartRequest{SessionID: id, Cwd: t.TempDir(), Env: env}
}

func startSession(t *testing.T, runtime *OMP, id, scenario, pidFile string) Snapshot {
	t.Helper()
	snapshot, err := runtime.Start(context.Background(), startRequest(t, id, scenario, pidFile))
	if err != nil {
		t.Fatalf("Start(%s): %v", id, err)
	}
	return snapshot
}

func waitFor(t *testing.T, timeout time.Duration, description string, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if condition() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", description)
}

func collectEvents(t *testing.T, events <-chan Event, timeout time.Duration, stop func(Event) bool) []Event {
	t.Helper()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	collected := make([]Event, 0, 32)
	for {
		select {
		case event, ok := <-events:
			if !ok {
				return collected
			}
			collected = append(collected, event)
			if stop != nil && stop(event) {
				return collected
			}
		case <-timer.C:
			t.Fatalf("timed out collecting events after %d events", len(collected))
		}
	}
}

func idleWithTurn(snapshot Snapshot, state State) bool {
	if snapshot.State != StateIdle || len(snapshot.Turns) == 0 {
		return false
	}
	return snapshot.Turns[len(snapshot.Turns)-1].State == state
}

func assistantText(snapshot Snapshot) string {
	var text strings.Builder
	for _, message := range snapshot.Messages {
		if message.Role == RoleAssistant {
			text.WriteString(message.Text)
		}
	}
	return text.String()
}

func readPIDFile(t *testing.T, path string) []int {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read pid file: %v", err)
	}
	var pids []int
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		if line == "" {
			continue
		}
		pid := 0
		for _, r := range line {
			if r < '0' || r > '9' {
				t.Fatalf("pid file contains %q", line)
			}
			pid = pid*10 + int(r-'0')
		}
		pids = append(pids, pid)
	}
	return pids
}

func processGone(pid int) bool {
	process, err := os.FindProcess(pid)
	if err != nil {
		return true
	}
	return process.Signal(syscall.Signal(0)) != nil
}

func TestStartHandshakeAndSnapshot(t *testing.T) {
	runtime := testRuntime(t, nil)
	snapshot := startSession(t, runtime, "s1", "text", "")

	if snapshot.Provider != ProviderNameOMP {
		t.Fatalf("provider = %q", snapshot.Provider)
	}
	if snapshot.Provenance != ProvenanceOMPRPC {
		t.Fatalf("provenance = %q", snapshot.Provenance)
	}
	if snapshot.ProviderSessionID != "mock-session-1" {
		t.Fatalf("provider session id = %q", snapshot.ProviderSessionID)
	}
	if snapshot.State != StateIdle {
		t.Fatalf("state = %q, want idle", snapshot.State)
	}
	if snapshot.Usage.Reported {
		t.Fatalf("usage must be explicitly unreported before a turn")
	}
	if len(snapshot.Messages) != 0 || len(snapshot.Turns) != 0 || len(snapshot.Tools) != 0 {
		t.Fatalf("fresh snapshot is not empty: %+v", snapshot)
	}
	if snapshot.PendingApprovals == nil || snapshot.Messages == nil || snapshot.Turns == nil || snapshot.Tools == nil {
		t.Fatalf("snapshot slices must be empty, not nil")
	}

	provider := runtime.Provider()
	if provider.Name != ProviderNameOMP {
		t.Fatalf("Provider().Name = %q", provider.Name)
	}
	capabilities := provider.Capabilities
	if !capabilities.StructuredTurns || !capabilities.Tools || !capabilities.Approvals || !capabilities.Usage || !capabilities.Steer {
		t.Fatalf("missing capabilities: %+v", capabilities)
	}
	if capabilities.Resume {
		t.Fatalf("resume must be explicitly unsupported")
	}
}

func TestPromptStreamsMessagesAndCompletes(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "text", "")
	events := runtime.Events("s1")

	if err := runtime.Prompt(context.Background(), "s1", "hello"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	collected := collectEvents(t, events, 10*time.Second, func(event Event) bool {
		return event.Kind == EventSnapshot && idleWithTurn(*event.Snapshot, StateDone)
	})

	sawDelta := false
	sawRunningSnapshot := false
	for _, event := range collected {
		switch event.Kind {
		case EventMessage:
			if event.Message.Role == RoleAssistant && strings.Contains(event.Message.Text, "OMP-") {
				sawDelta = true
			}
		case EventSnapshot:
			if event.Snapshot.State == StateRunning {
				sawRunningSnapshot = true
			}
		}
	}
	if !sawDelta {
		t.Fatalf("no streaming assistant message event")
	}
	if !sawRunningSnapshot {
		t.Fatalf("no running snapshot event")
	}

	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if snapshot.State != StateIdle {
		t.Fatalf("state = %q", snapshot.State)
	}
	if len(snapshot.Turns) != 1 || snapshot.Turns[0].State != StateDone || snapshot.Turns[0].EndedMS == 0 {
		t.Fatalf("turn = %+v", snapshot.Turns)
	}
	if got := assistantText(snapshot); got != "OMP-MOCK" {
		t.Fatalf("assistant text = %q", got)
	}
	if snapshot.Messages[0].Role != RoleUser || snapshot.Messages[0].Text != "hello" {
		t.Fatalf("user message = %+v", snapshot.Messages[0])
	}
	if !snapshot.Usage.Reported {
		t.Fatalf("usage not reported")
	}
	if snapshot.Usage.InputTokens != 10 || snapshot.Usage.OutputTokens != 5 || snapshot.Usage.TotalTokens != 17 {
		t.Fatalf("usage = %+v", snapshot.Usage)
	}
	if snapshot.Usage.CostUSD != 0.001 {
		t.Fatalf("cost = %v", snapshot.Usage.CostUSD)
	}
}

func TestToolEvents(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "tool", "")
	events := runtime.Events("s1")
	if err := runtime.Prompt(context.Background(), "s1", "run a tool"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	collectEvents(t, events, 10*time.Second, func(event Event) bool {
		return event.Kind == EventSnapshot && idleWithTurn(*event.Snapshot, StateDone)
	})

	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if len(snapshot.Tools) != 1 {
		t.Fatalf("tools = %+v", snapshot.Tools)
	}
	tool := snapshot.Tools[0]
	if tool.ID != "tool-1" || tool.Name != "bash" {
		t.Fatalf("tool identity = %+v", tool)
	}
	if tool.Status != ToolCompleted || tool.IsError {
		t.Fatalf("tool status = %+v", tool)
	}
	if tool.TurnID != snapshot.Turns[0].ID {
		t.Fatalf("tool turn id = %q, want %q", tool.TurnID, snapshot.Turns[0].ID)
	}
	if !strings.Contains(string(tool.Input), "echo hi") {
		t.Fatalf("tool input = %s", tool.Input)
	}
	if !strings.Contains(string(tool.Output), "hi") {
		t.Fatalf("tool output = %s", tool.Output)
	}
	if tool.StartedMS == 0 || tool.EndedMS == 0 {
		t.Fatalf("tool timestamps missing: %+v", tool)
	}
}

func TestApprovalBlocksTurnAndApproveContinues(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "approval", "")
	events := runtime.Events("s1")
	if err := runtime.Prompt(context.Background(), "s1", "needs approval"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}

	var approval Approval
	collectEvents(t, events, 10*time.Second, func(event Event) bool {
		if event.Kind == EventApproval && event.Approval.State == ApprovalPending {
			approval = *event.Approval
			return true
		}
		return false
	})

	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if snapshot.State != StateBlocked {
		t.Fatalf("state = %q, want blocked", snapshot.State)
	}
	if len(snapshot.PendingApprovals) != 1 {
		t.Fatalf("pending approvals = %+v", snapshot.PendingApprovals)
	}
	if snapshot.PendingApprovals[0].ToolName != "bash" {
		t.Fatalf("approval tool name = %q", snapshot.PendingApprovals[0].ToolName)
	}
	if snapshot.Turns[0].State != StateBlocked {
		t.Fatalf("turn state = %q, want blocked", snapshot.Turns[0].State)
	}
	if approval.TurnID == "" || approval.ID != "approval-1" {
		t.Fatalf("approval = %+v", approval)
	}

	if err := runtime.Approve(context.Background(), "s1", approval.ID, DecisionApprove, ""); err != nil {
		t.Fatalf("Approve: %v", err)
	}
	collectEvents(t, events, 10*time.Second, func(event Event) bool {
		return event.Kind == EventSnapshot && idleWithTurn(*event.Snapshot, StateDone)
	})

	snapshot, err = runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if len(snapshot.PendingApprovals) != 0 {
		t.Fatalf("approval still pending: %+v", snapshot.PendingApprovals)
	}
	if got := assistantText(snapshot); got != "APPROVED" {
		t.Fatalf("assistant text = %q", got)
	}
	if len(snapshot.Tools) != 1 || snapshot.Tools[0].Status != ToolCompleted {
		t.Fatalf("tool = %+v", snapshot.Tools)
	}

	// A second answer to the same approval is a typed error, not a no-op.
	err = runtime.Approve(context.Background(), "s1", approval.ID, DecisionApprove, "")
	if CodeOf(err) != CodeApprovalSettled && CodeOf(err) != CodeApprovalNotFound {
		t.Fatalf("second Approve error = %v", err)
	}
}

func TestAbortYieldsCancelled(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "hold", "")
	if err := runtime.Prompt(context.Background(), "s1", "start"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	waitFor(t, 10*time.Second, "running turn", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateRunning && len(snapshot.Turns) == 1 && snapshot.Turns[0].State == StateRunning
	})

	if err := runtime.Abort(context.Background(), "s1"); err != nil {
		t.Fatalf("Abort: %v", err)
	}
	waitFor(t, 10*time.Second, "cancelled turn", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && idleWithTurn(snapshot, StateCancelled)
	})
}

func TestIdleAdmissionRejectsSecondPrompt(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "hold", "")
	events := runtime.Events("s1")
	if err := runtime.Prompt(context.Background(), "s1", "first"); err != nil {
		t.Fatalf("first Prompt: %v", err)
	}
	waitFor(t, 10*time.Second, "running turn", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateRunning
	})

	if err := runtime.Prompt(context.Background(), "s1", "second"); CodeOf(err) != CodeBusy {
		t.Fatalf("second Prompt error = %v, want busy", err)
	}

	// No prompt may reach the child while the turn is active. Give the helper
	// time to emit its DOUBLE-PROMPT marker if one did.
	deadline := time.After(300 * time.Millisecond)
drain:
	for {
		select {
		case event, ok := <-events:
			if !ok {
				break drain
			}
			if event.Kind == EventDiagnostic && event.Error != nil && strings.Contains(event.Error.Message, "DOUBLE-PROMPT") {
				t.Fatalf("a second prompt reached the child: %+v", event.Error)
			}
		case <-deadline:
			break drain
		}
	}

	if err := runtime.Abort(context.Background(), "s1"); err != nil {
		t.Fatalf("Abort: %v", err)
	}
}

func TestConcurrentPromptsAdmitExactlyOne(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "hold", "")
	waitFor(t, 10*time.Second, "idle session", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateIdle
	})

	const attempts = 8
	results := make([]error, attempts)
	var wait sync.WaitGroup
	for index := range attempts {
		wait.Add(1)
		go func(index int) {
			defer wait.Done()
			results[index] = runtime.Prompt(context.Background(), "s1", "race")
		}(index)
	}
	wait.Wait()

	successes, busy := 0, 0
	for _, err := range results {
		switch CodeOf(err) {
		case "":
			successes++
		case CodeBusy:
			busy++
		default:
			t.Fatalf("unexpected Prompt error: %v", err)
		}
	}
	if successes != 1 || busy != attempts-1 {
		t.Fatalf("successes=%d busy=%d", successes, busy)
	}
	if err := runtime.Abort(context.Background(), "s1"); err != nil {
		t.Fatalf("Abort: %v", err)
	}
}

func TestSessionLimit(t *testing.T) {
	runtime := testRuntime(t, func(cfg *Config) { cfg.MaxSessions = 4 })
	for _, id := range []string{"s1", "s2", "s3", "s4"} {
		startSession(t, runtime, id, "hold", "")
	}

	_, err := runtime.Start(context.Background(), startRequest(t, "s5", "hold", ""))
	if CodeOf(err) != CodeLimitExceeded {
		t.Fatalf("fifth Start error = %v, want limit_exceeded", err)
	}

	if err := runtime.Stop(context.Background(), "s1"); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	startSession(t, runtime, "s5", "hold", "")
	if got := len(runtime.List()); got != 5 {
		t.Fatalf("List length = %d", got)
	}
}

func TestChildExitSurfacesFailed(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "exit", "")
	events := runtime.Events("s1")

	err := runtime.Prompt(context.Background(), "s1", "die")
	if err == nil {
		t.Fatalf("Prompt succeeded against an exiting child")
	}
	waitFor(t, 10*time.Second, "failed session", func() bool {
		snapshot, snapshotErr := runtime.Snapshot(context.Background(), "s1")
		return snapshotErr == nil && snapshot.State == StateFailed
	})
	snapshot, snapshotErr := runtime.Snapshot(context.Background(), "s1")
	if snapshotErr != nil {
		t.Fatalf("Snapshot: %v", snapshotErr)
	}
	if snapshot.Error == nil {
		t.Fatalf("failed snapshot has no error")
	}
	collected := collectEvents(t, events, 10*time.Second, func(Event) bool { return false })
	sawError := false
	for _, event := range collected {
		if event.Kind == EventError && event.Error != nil {
			sawError = true
		}
	}
	if !sawError {
		t.Fatalf("no error event was emitted")
	}
	if err := runtime.Stop(context.Background(), "s1"); err != nil {
		t.Fatalf("Stop after failure: %v", err)
	}
}

func TestMalformedFrameKillsAndReapsChild(t *testing.T) {
	runtime := testRuntime(t, nil)
	pidFile := filepath.Join(t.TempDir(), "pids")
	startSession(t, runtime, "s1", "malformed", pidFile)
	events := runtime.Events("s1")

	// The acknowledgement may or may not win the race against the malformed
	// frame; either way the session must settle as a protocol failure.
	_ = runtime.Prompt(context.Background(), "s1", "break")
	waitFor(t, 10*time.Second, "failed session", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateFailed
	})
	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if snapshot.Error == nil || snapshot.Error.Code != CodeProtocol {
		t.Fatalf("error = %+v, want protocol", snapshot.Error)
	}
	pids := readPIDFile(t, pidFile)
	if len(pids) != 1 {
		t.Fatalf("pid file = %v", pids)
	}
	waitFor(t, 10*time.Second, "child reaped", func() bool { return processGone(pids[0]) })
	collectEvents(t, events, 10*time.Second, func(Event) bool { return false })
	if err := runtime.Stop(context.Background(), "s1"); err != nil {
		t.Fatalf("Stop: %v", err)
	}
}

func TestStderrFailureKillsAndReapsChild(t *testing.T) {
	runtime := testRuntime(t, nil)
	pidFile := filepath.Join(t.TempDir(), "pids")
	startSession(t, runtime, "s1", "stderr-flood", pidFile)
	_ = runtime.Prompt(context.Background(), "s1", "flood")

	waitFor(t, 10*time.Second, "failed session", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateFailed
	})
	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if snapshot.Error == nil || !strings.Contains(snapshot.Error.Message, "stderr") {
		t.Fatalf("error = %+v, want a stderr diagnostic", snapshot.Error)
	}
	pids := readPIDFile(t, pidFile)
	waitFor(t, 10*time.Second, "child reaped", func() bool { return processGone(pids[0]) })
}

func TestStopIsIdempotentAndRestartable(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "hold", "")
	if err := runtime.Stop(context.Background(), "s1"); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot after Stop: %v", err)
	}
	if snapshot.State != StateDone {
		t.Fatalf("state after Stop = %q", snapshot.State)
	}
	if err := runtime.Stop(context.Background(), "s1"); err != nil {
		t.Fatalf("second Stop: %v", err)
	}

	events := runtime.Events("s1")
	collectEvents(t, events, 10*time.Second, func(Event) bool { return false })

	startSession(t, runtime, "s1", "text", "")
	if err := runtime.Prompt(context.Background(), "s1", "again"); err != nil {
		t.Fatalf("Prompt after restart: %v", err)
	}
	waitFor(t, 10*time.Second, "completed restarted turn", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && idleWithTurn(snapshot, StateDone)
	})
}

func TestCloseStopsAllSessions(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "hold", "")
	startSession(t, runtime, "s2", "hold", "")

	if err := runtime.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	for _, snapshot := range runtime.List() {
		if snapshot.State != StateDone {
			t.Fatalf("session %s state = %q", snapshot.ProviderSessionID, snapshot.State)
		}
	}
	if err := runtime.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if _, err := runtime.Start(context.Background(), startRequest(t, "s3", "text", "")); CodeOf(err) != CodeClosed {
		t.Fatalf("Start after Close error = %v", err)
	}
}

func TestStartupTimeoutKillsChild(t *testing.T) {
	runtime := testRuntime(t, func(cfg *Config) { cfg.StartupTimeout = 300 * time.Millisecond })
	pidFile := filepath.Join(t.TempDir(), "pids")
	_, err := runtime.Start(context.Background(), startRequest(t, "s1", "noready", pidFile))
	if CodeOf(err) != CodeTimeout {
		t.Fatalf("Start error = %v, want timeout", err)
	}
	pids := readPIDFile(t, pidFile)
	waitFor(t, 10*time.Second, "child reaped", func() bool { return processGone(pids[0]) })
}

func TestErrorsRedactSecrets(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "stderr-secret", "")
	_ = runtime.Prompt(context.Background(), "s1", "fail")

	waitFor(t, 10*time.Second, "failed session", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateFailed
	})
	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if snapshot.Error == nil {
		t.Fatalf("failed snapshot has no error")
	}
	if strings.Contains(snapshot.Error.Message, fakeCredential) {
		t.Fatalf("error leaked a credential: %s", snapshot.Error.Message)
	}
	if !strings.Contains(snapshot.Error.Message, redactedPlaceholder) {
		t.Fatalf("error did not redact the credential: %s", snapshot.Error.Message)
	}
}

func TestBridgeDisconnectDoesNotStopChild(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "burst", "")
	// Subscribe and never read: a bridge/window disconnect must not stop the
	// child or block its stdout.
	_ = runtime.Events("s1")

	if err := runtime.Prompt(context.Background(), "s1", "burst"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	waitFor(t, 20*time.Second, "burst completion", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && idleWithTurn(snapshot, StateDone)
	})
	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if got := len(assistantText(snapshot)); got != 600 {
		t.Fatalf("assistant text length = %d", got)
	}
	if err := runtime.Prompt(context.Background(), "s1", "again"); err != nil {
		t.Fatalf("child stopped after subscriber disconnect: %v", err)
	}
	waitFor(t, 20*time.Second, "second burst completion", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateIdle && len(snapshot.Turns) == 2 && snapshot.Turns[1].State == StateDone
	})
}

func TestStartIsIdempotent(t *testing.T) {
	runtime := testRuntime(t, nil)
	pidFile := filepath.Join(t.TempDir(), "pids")
	first := startSession(t, runtime, "s1", "hold", pidFile)
	second := startSession(t, runtime, "s1", "hold", pidFile)
	if first.ProviderSessionID != second.ProviderSessionID {
		t.Fatalf("provider session changed: %q vs %q", first.ProviderSessionID, second.ProviderSessionID)
	}
	pids := readPIDFile(t, pidFile)
	if len(pids) != 1 {
		t.Fatalf("Start spawned %d children, want 1: %v", len(pids), pids)
	}
}

func TestUnknownSessionAndResume(t *testing.T) {
	runtime := testRuntime(t, nil)
	if _, err := runtime.Snapshot(context.Background(), "missing"); CodeOf(err) != CodeSessionNotFound {
		t.Fatalf("Snapshot error = %v", err)
	}
	if err := runtime.Prompt(context.Background(), "missing", "x"); CodeOf(err) != CodeSessionNotFound {
		t.Fatalf("Prompt error = %v", err)
	}
	if err := runtime.Stop(context.Background(), "missing"); CodeOf(err) != CodeSessionNotFound {
		t.Fatalf("Stop error = %v", err)
	}
	if err := runtime.Approve(context.Background(), "missing", "a", DecisionApprove, ""); CodeOf(err) != CodeSessionNotFound {
		t.Fatalf("Approve error = %v", err)
	}
	events := runtime.Events("missing")
	if _, ok := <-events; ok {
		t.Fatalf("unknown session event channel is not closed")
	}
	if _, err := runtime.Resume(context.Background(), "missing", "provider"); CodeOf(err) != CodeUnsupported {
		t.Fatalf("Resume error = %v", err)
	}
}

func TestSteerIsAcknowledged(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "hold", "")
	if err := runtime.Prompt(context.Background(), "s1", "start"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	waitFor(t, 10*time.Second, "running turn", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && snapshot.State == StateRunning
	})
	if err := runtime.Steer(context.Background(), "s1", "also check the tests"); err != nil {
		t.Fatalf("Steer: %v", err)
	}
	if err := runtime.Abort(context.Background(), "s1"); err != nil {
		t.Fatalf("Abort: %v", err)
	}
}

func TestApprovalDenySettlesTurn(t *testing.T) {
	runtime := testRuntime(t, nil)
	startSession(t, runtime, "s1", "approval", "")
	events := runtime.Events("s1")
	if err := runtime.Prompt(context.Background(), "s1", "needs approval"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	var approvalID string
	collectEvents(t, events, 10*time.Second, func(event Event) bool {
		if event.Kind == EventApproval && event.Approval.State == ApprovalPending {
			approvalID = event.Approval.ID
			return true
		}
		return false
	})
	if err := runtime.Approve(context.Background(), "s1", approvalID, DecisionDeny, "not now"); err != nil {
		t.Fatalf("Approve: %v", err)
	}
	collectEvents(t, events, 10*time.Second, func(event Event) bool {
		return event.Kind == EventSnapshot && idleWithTurn(*event.Snapshot, StateDone)
	})
	snapshot, err := runtime.Snapshot(context.Background(), "s1")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if got := assistantText(snapshot); got != "DENIED" {
		t.Fatalf("assistant text = %q", got)
	}
	if len(snapshot.Tools) != 1 || !snapshot.Tools[0].IsError {
		t.Fatalf("denied tool = %+v", snapshot.Tools)
	}
}

func TestEventJSONIsStable(t *testing.T) {
	event := Event{
		Kind:      EventSnapshot,
		SessionID: "s1",
		Sequence:  3,
		Snapshot: &Snapshot{
			Provider:   ProviderNameOMP,
			State:      StateIdle,
			Provenance: ProvenanceOMPRPC,
			Usage:      Usage{Reported: false},
		},
	}
	data, err := json.Marshal(event)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	for _, key := range []string{`"kind":"snapshot"`, `"sessionId":"s1"`, `"sequence":3`, `"provider":"omp"`, `"provenance":"omp-rpc"`, `"reported":false`} {
		if !strings.Contains(string(data), key) {
			t.Fatalf("missing %s in %s", key, data)
		}
	}
}

// TestLiveOMP is the real-omp smoke test. It is skipped unless UKK_OMP_LIVE=1.
func TestLiveOMP(t *testing.T) {
	if os.Getenv("UKK_OMP_LIVE") != "1" {
		t.Skip("set UKK_OMP_LIVE=1 to run against the real omp binary")
	}
	runtime, err := NewOMP(Config{
		StartupTimeout: 90 * time.Second,
		RequestTimeout: 60 * time.Second,
		ShutdownGrace:  3 * time.Second,
	})
	if err != nil {
		t.Fatalf("NewOMP: %v", err)
	}
	defer func() { _ = runtime.Close() }()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	snapshot, err := runtime.Start(ctx, StartRequest{SessionID: "live", Cwd: t.TempDir()})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}
	if snapshot.State != StateIdle {
		t.Fatalf("state = %q", snapshot.State)
	}
	events := runtime.Events("live")
	if err := runtime.Prompt(ctx, "live", "Reply with exactly: OMP-OK"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	collectEvents(t, events, 4*time.Minute, func(event Event) bool {
		return event.Kind == EventSnapshot && idleWithTurn(*event.Snapshot, StateDone)
	})
	final, err := runtime.Snapshot(ctx, "live")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if !strings.Contains(assistantText(final), "OMP-OK") {
		t.Fatalf("assistant text = %q", assistantText(final))
	}
	if err := runtime.Stop(ctx, "live"); err != nil {
		t.Fatalf("Stop: %v", err)
	}
}
