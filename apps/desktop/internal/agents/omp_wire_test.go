package agents

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestParseReady(t *testing.T) {
	ready := []byte(`{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864,"supportsBashExcludeFromContext":true}`)
	info, err := parseReady(ready)
	if err != nil {
		t.Fatalf("parseReady: %v", err)
	}
	if info.protocolVersion != 1 || info.maxFrameBytes != 1048576 || !info.supportsBashExclusion {
		t.Fatalf("info = %+v", info)
	}

	cases := map[string]string{
		"unsupported version": `{"type":"ready","protocolVersion":2,"maxFrameBytes":1048576}`,
		"missing version":     `{"type":"ready","maxFrameBytes":1048576}`,
		"missing frame cap":   `{"type":"ready","protocolVersion":1}`,
		"unusable frame cap":  `{"type":"ready","protocolVersion":1,"maxFrameBytes":16}`,
		"wrong type":          `{"type":"response","protocolVersion":1,"maxFrameBytes":1048576}`,
		"no v1 support":       `{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[2],"maxFrameBytes":1048576}`,
		"not json":            `not json`,
	}
	for name, payload := range cases {
		if _, err := parseReady([]byte(payload)); err == nil {
			t.Fatalf("%s: expected an error", name)
		}
	}
}

func TestApprovalRecognitionAndAnswer(t *testing.T) {
	selectFrame := &uiRequestFrame{
		Type:    "extension_ui_request",
		ID:      "approval-1",
		Method:  "select",
		Title:   "Allow tool: bash\nArguments:\n{\"command\":\"ls\"}",
		Options: []string{"Approve File", "Always Approve File", "Approve Once", "Deny"},
	}
	approval, ok := approvalFromUI(selectFrame, 1000)
	if !ok {
		t.Fatalf("select approval not recognized")
	}
	if approval.ID != "approval-1" || approval.ToolName != "bash" || approval.State != ApprovalPending {
		t.Fatalf("approval = %+v", approval)
	}

	answer, err := approvalAnswer("select", selectFrame.Options, DecisionApprove)
	if err != nil {
		t.Fatalf("approvalAnswer: %v", err)
	}
	if answer.Value == nil || *answer.Value != "Approve Once" {
		t.Fatalf("approve answer = %+v", answer)
	}
	deny, err := approvalAnswer("select", selectFrame.Options, DecisionDeny)
	if err != nil {
		t.Fatalf("deny: %v", err)
	}
	if deny.Value == nil || *deny.Value != "Deny" {
		t.Fatalf("deny answer = %+v", deny)
	}
	cancel, err := approvalAnswer("select", selectFrame.Options, DecisionCancel)
	if err != nil || cancel.Cancelled == nil || !*cancel.Cancelled {
		t.Fatalf("cancel answer = %+v, %v", cancel, err)
	}

	// A plain select dialog is not an approval.
	if _, ok := approvalFromUI(&uiRequestFrame{Type: "extension_ui_request", ID: "x", Method: "select", Title: "Pick one", Options: []string{"a", "b"}}, 0); ok {
		t.Fatalf("plain select treated as an approval")
	}

	confirm := &uiRequestFrame{Type: "extension_ui_request", ID: "c1", Method: "confirm", Title: "Allow tool: edit"}
	confirmApproval, ok := approvalFromUI(confirm, 0)
	if !ok || confirmApproval.ToolName != "edit" {
		t.Fatalf("confirm approval = %+v, %v", confirmApproval, ok)
	}
	confirmed, err := approvalAnswer("confirm", nil, DecisionApprove)
	if err != nil || confirmed.Confirmed == nil || !*confirmed.Confirmed {
		t.Fatalf("confirm answer = %+v, %v", confirmed, err)
	}
}

func TestApprovalExpiryUsesAdvertisedTimeout(t *testing.T) {
	frame := &uiRequestFrame{
		Type: "extension_ui_request", ID: "a", Method: "select",
		Title: "Allow tool: bash", Options: []string{"Approve", "Deny"}, TimeoutMS: 1500,
	}
	approval, ok := approvalFromUI(frame, 10_000)
	if !ok {
		t.Fatalf("approval not recognized")
	}
	if approval.ExpiresMS != 11_500 {
		t.Fatalf("expires = %d", approval.ExpiresMS)
	}
}

func TestReadBoundedLine(t *testing.T) {
	reader := bufio.NewReaderSize(strings.NewReader("short\n"+strings.Repeat("x", 100)+"\n"), 8)
	line, oversized, err := readBoundedLine(reader, 32, 64)
	if err != nil || oversized || string(line) != "short" {
		t.Fatalf("line=%q oversized=%v err=%v", line, oversized, err)
	}
	line, oversized, err = readBoundedLine(reader, 32, 256)
	if err != nil || !oversized || len(line) != 0 {
		t.Fatalf("oversized line=%q oversized=%v err=%v", line, oversized, err)
	}

	reader = bufio.NewReaderSize(strings.NewReader(strings.Repeat("y", 200)), 8)
	if _, _, err := readBoundedLine(reader, 32, 64); !errors.Is(err, errFrameTooLong) {
		t.Fatalf("drain error = %v", err)
	}
}

func TestValidateStartRequest(t *testing.T) {
	if err := validateStartRequest(StartRequest{}); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("empty id error = %v", err)
	}
	dir := testDir(t)
	if err := validateStartRequest(StartRequest{SessionID: "s", Cwd: dir}); err != nil {
		t.Fatalf("valid request: %v", err)
	}
	if err := validateStartRequest(StartRequest{SessionID: "s", Cwd: "relative"}); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("relative cwd error = %v", err)
	}
	if err := validateStartRequest(StartRequest{SessionID: "s", Cwd: filepath.Join(dir, "missing")}); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("missing cwd error = %v", err)
	}
	if err := validateStartRequest(StartRequest{SessionID: "s", Env: map[string]string{"A=B": "c"}}); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("invalid env error = %v", err)
	}
	if err := validateStartRequest(StartRequest{SessionID: "s\x00"}); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("control character error = %v", err)
	}
}

func TestUsageFromFrameIsExplicit(t *testing.T) {
	if _, reported := usageFromFrame(nil); reported {
		t.Fatalf("nil usage reported")
	}
	frame := &usageFrame{Input: 1, Output: 2, TotalTokens: 3}
	frame.Cost.Total = 0.5
	usage, reported := usageFromFrame(frame)
	if !reported || !usage.Reported || usage.InputTokens != 1 || usage.OutputTokens != 2 || usage.CostUSD != 0.5 {
		t.Fatalf("usage = %+v", usage)
	}
}

func TestMessageTextExcludesThinking(t *testing.T) {
	raw := json.RawMessage(`[{"type":"thinking","thinking":"secret"},{"type":"text","text":"hello"},{"type":"text","text":" world"}]`)
	if got := messageText(raw); got != "hello world" {
		t.Fatalf("messageText = %q", got)
	}
	if got := messageText(json.RawMessage(`"plain"`)); got != "plain" {
		t.Fatalf("plain messageText = %q", got)
	}
}

func TestMergedEnvOverridesDeterministically(t *testing.T) {
	t.Setenv("UKK_AGENTS_TEST_ENV", "original")
	env := mergedEnv(map[string]string{"UKK_AGENTS_TEST_ENV": "override", "UKK_AGENTS_EXTRA": "1"})
	var found string
	var extra string
	for _, entry := range env {
		if strings.HasPrefix(entry, "UKK_AGENTS_TEST_ENV=") {
			found = entry
		}
		if strings.HasPrefix(entry, "UKK_AGENTS_EXTRA=") {
			extra = entry
		}
	}
	if found != "UKK_AGENTS_TEST_ENV=override" {
		t.Fatalf("override = %q", found)
	}
	if extra != "UKK_AGENTS_EXTRA=1" {
		t.Fatalf("extra = %q", extra)
	}
}

func TestDefaultConfigArguments(t *testing.T) {
	cfg, err := (Config{}).normalized()
	if err != nil {
		t.Fatalf("normalized: %v", err)
	}
	if cfg.Binary != "omp" || strings.Join(cfg.Args, " ") != "--mode rpc --no-session" {
		t.Fatalf("cfg = %+v", cfg)
	}
	if cfg.MaxSessions != defaultMaxSessions {
		t.Fatalf("MaxSessions = %d", cfg.MaxSessions)
	}
	if _, err := (Config{MaxSessions: -1}).normalized(); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("negative MaxSessions error = %v", err)
	}
	if _, err := (Config{MaxFrameBytes: 16}).normalized(); CodeOf(err) != CodeInvalidArgument {
		t.Fatalf("tiny frame cap error = %v", err)
	}
}

func TestStartArgs(t *testing.T) {
	base := []string{"--mode", "rpc", "--no-session"}
	got := startArgs(base, StartRequest{Model: "openai/gpt-5", Profile: "work"})
	want := []string{"--mode", "rpc", "--no-session", "--model", "openai/gpt-5", "--profile", "work"}
	if strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("args = %v", got)
	}
	if len(startArgs(base, StartRequest{})) != len(base) {
		t.Fatalf("empty request changed args")
	}
	if len(base) != 3 || base[2] != "--no-session" {
		t.Fatalf("startArgs mutated its input: %v", base)
	}
}

func TestEventHistoryIsBounded(t *testing.T) {
	runtime := testRuntime(t, func(cfg *Config) {
		cfg.EventHistory = 16
		cfg.SubscriberQueue = 4
	})
	startSession(t, runtime, "s1", "burst", "")
	if err := runtime.Prompt(context.Background(), "s1", "burst"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	waitFor(t, 20*time.Second, "burst completion", func() bool {
		snapshot, err := runtime.Snapshot(context.Background(), "s1")
		return err == nil && idleWithTurn(snapshot, StateDone)
	})

	events := runtime.Events("s1")
	replayed := 0
	for {
		select {
		case _, ok := <-events:
			if !ok {
				t.Fatalf("session closed during replay")
			}
			replayed++
			if replayed > 16 {
				t.Fatalf("replayed %d events, history is not bounded", replayed)
			}
		default:
			if replayed == 0 {
				t.Fatalf("no events replayed")
			}
			return
		}
	}
}

func TestPIDFileParsingHelper(t *testing.T) {
	path := filepath.Join(testDir(t), "pids")
	if err := os.WriteFile(path, []byte("12\n34\n"), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
	pids := readPIDFile(t, path)
	if len(pids) != 2 || pids[0] != 12 || pids[1] != 34 {
		t.Fatalf("pids = %v", pids)
	}
}
