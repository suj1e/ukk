package agents

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"
)

// TestLiveOMPApproval verifies the real omp approval transport: RPC v1 carries
// a tool approval as an extension UI `select`, not as a dedicated frame. It is
// opt-in with the same UKK_OMP_LIVE=1 guard as the text smoke test.
func TestLiveOMPApproval(t *testing.T) {
	if os.Getenv("UKK_OMP_LIVE") != "1" {
		t.Skip("set UKK_OMP_LIVE=1 to run against the real omp binary")
	}
	runtime, err := NewOMP(Config{
		Args:           []string{"--mode", "rpc", "--no-session", "--approval-mode", "always-ask"},
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
	if _, err := runtime.Start(ctx, StartRequest{SessionID: "live-approval", Cwd: t.TempDir()}); err != nil {
		t.Fatalf("Start: %v", err)
	}
	events := runtime.Events("live-approval")
	if err := runtime.Prompt(ctx, "live-approval", "Use the bash tool to run `printf LIVE-APPROVAL` in the current directory. Do not ask anything else."); err != nil {
		t.Fatalf("Prompt: %v", err)
	}

	var approval Approval
	collectEvents(t, events, 4*time.Minute, func(event Event) bool {
		if event.Kind == EventApproval && event.Approval.State == ApprovalPending {
			approval = *event.Approval
			return true
		}
		return false
	})
	t.Logf("approval: id=%s tool=%s turn=%s expires=%d", approval.ID, approval.ToolName, approval.TurnID, approval.ExpiresMS)
	if approval.ID == "" {
		t.Fatalf("approval has no id")
	}
	if err := runtime.Approve(ctx, "live-approval", approval.ID, DecisionApprove, ""); err != nil {
		t.Fatalf("Approve: %v", err)
	}
	collectEvents(t, events, 4*time.Minute, func(event Event) bool {
		return event.Kind == EventSnapshot && idleWithTurn(*event.Snapshot, StateDone)
	})
	final, err := runtime.Snapshot(ctx, "live-approval")
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if !strings.Contains(assistantText(final), "LIVE-APPROVAL") {
		t.Fatalf("assistant text = %q", assistantText(final))
	}
}
