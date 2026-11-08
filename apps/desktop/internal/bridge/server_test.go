package bridge

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

type fakeAgents struct {
	mu      sync.Mutex
	snap    AgentSnapshot
	started []StartParams
	prompts []PromptParams
	aborted int
	stopped int
	subs    map[string]chan AgentSnapshot
}

func newFakeAgents() *fakeAgents {
	return &fakeAgents{
		snap: AgentSnapshot{Provider: "omp", State: "idle", Provenance: "omp-rpc"},
		subs: map[string]chan AgentSnapshot{},
	}
}

func (f *fakeAgents) Start(_ context.Context, params StartParams) (AgentSnapshot, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.started = append(f.started, params)
	f.snap.Provider = "omp"
	f.snap.State = "idle"
	return f.snap, nil
}

func (f *fakeAgents) Snapshot(_ context.Context, sessionID string) (AgentSnapshot, error) {
	if sessionID == "missing" {
		return AgentSnapshot{}, &Error{Code: "not_found", Message: "no session"}
	}
	return f.snap, nil
}

func (f *fakeAgents) List(context.Context) []AgentSnapshot { return []AgentSnapshot{f.snap} }

func (f *fakeAgents) Prompt(_ context.Context, params PromptParams) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if params.Text == "" {
		return &Error{Code: "invalid_params", Message: "empty prompt"}
	}
	f.prompts = append(f.prompts, params)
	f.snap.State = "running"
	return nil
}

func (f *fakeAgents) Abort(context.Context, string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.aborted++
	return nil
}

func (f *fakeAgents) Stop(context.Context, string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.stopped++
	return nil
}

func (f *fakeAgents) Approve(_ context.Context, params ApproveParams) error {
	if params.Decision != "approve" && params.Decision != "deny" {
		return &Error{Code: "invalid_params", Message: "bad decision"}
	}
	return nil
}

func (f *fakeAgents) Subscribe(sessionID string) (<-chan AgentSnapshot, func()) {
	ch := make(chan AgentSnapshot, 4)
	f.mu.Lock()
	f.subs[sessionID] = ch
	f.mu.Unlock()
	return ch, func() {
		f.mu.Lock()
		delete(f.subs, sessionID)
		f.mu.Unlock()
		close(ch)
	}
}

type fakeWorkspace struct {
	mu   sync.Mutex
	data json.RawMessage
}

func (f *fakeWorkspace) Snapshot() json.RawMessage {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.data
}

func (f *fakeWorkspace) Replace(raw json.RawMessage) error {
	var probe map[string]any
	if err := json.Unmarshal(raw, &probe); err != nil {
		return err
	}
	f.mu.Lock()
	f.data = append(json.RawMessage(nil), raw...)
	f.mu.Unlock()
	return nil
}

func newTestServer(t *testing.T) (*Server, *fakeAgents) {
	t.Setenv("UKK_DATA_DIR", t.TempDir())
	agents := newFakeAgents()
	server, err := NewServer(ServerOptions{Agents: agents, Workspace: &fakeWorkspace{}})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(server.Close)
	return server, agents
}

func TestAuthAndHealth(t *testing.T) {
	server, _ := newTestServer(t)
	httpServer := httptest.NewServer(server.Handler("secret"))
	defer httpServer.Close()

	request, _ := http.NewRequest("POST", httpServer.URL+"/v1/command", strings.NewReader(`{"id":"1","method":"health"}`))
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	response.Body.Close()
	if response.StatusCode != 401 {
		t.Fatalf("status without token = %d", response.StatusCode)
	}

	request, _ = http.NewRequest("POST", httpServer.URL+"/v1/command", strings.NewReader(`{"id":"1","method":"health"}`))
	request.Header.Set("Authorization", "Bearer secret")
	response, err = http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	var decoded Response
	if err := json.NewDecoder(response.Body).Decode(&decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.ID != "1" || decoded.Error != nil || len(decoded.Result) == 0 {
		t.Fatalf("response = %+v", decoded)
	}
}

func TestDispatchAgentFlow(t *testing.T) {
	server, agents := newTestServer(t)
	ctx := context.Background()

	start, err := server.Dispatch(ctx, MethodAgentStart, json.RawMessage(`{"session_id":"s1","cwd":"/tmp"}`), AllowAll{})
	if err != nil {
		t.Fatal(err)
	}
	if snapshot := start.(AgentSnapshot); snapshot.Provider != "omp" {
		t.Fatalf("start = %+v", snapshot)
	}
	if len(agents.started) != 1 || agents.started[0].Cwd != "/tmp" {
		t.Fatalf("started = %+v", agents.started)
	}
	if _, err := server.Dispatch(ctx, MethodAgentPrompt, json.RawMessage(`{"session_id":"s1","text":"hi"}`), AllowAll{}); err != nil {
		t.Fatal(err)
	}
	if len(agents.prompts) != 1 {
		t.Fatalf("prompts = %+v", agents.prompts)
	}
	if _, err := server.Dispatch(ctx, MethodAgentApprove, json.RawMessage(`{"session_id":"s1","approval_id":"a","decision":"maybe"}`), AllowAll{}); err == nil {
		t.Fatal("invalid decision accepted")
	}
	if _, err := server.Dispatch(ctx, MethodAgentApprove, json.RawMessage(`{"session_id":"s1","approval_id":"a","decision":"approve"}`), AllowAll{}); err != nil {
		t.Fatal(err)
	}
	if _, err := server.Dispatch(ctx, MethodAgentAbort, json.RawMessage(`{"session_id":"s1"}`), AllowAll{}); err != nil {
		t.Fatal(err)
	}
	if _, err := server.Dispatch(ctx, MethodAgentStop, json.RawMessage(`{"session_id":"s1"}`), AllowAll{}); err != nil {
		t.Fatal(err)
	}
	if agents.aborted != 1 || agents.stopped != 1 {
		t.Fatalf("aborted=%d stopped=%d", agents.aborted, agents.stopped)
	}
}

func TestDispatchRejectsUnknownFieldAndMethod(t *testing.T) {
	server, _ := newTestServer(t)
	ctx := context.Background()
	_, err := server.Dispatch(ctx, MethodAgentStart, json.RawMessage(`{"session_id":"s1","cwd":"/tmp","surprise":1}`), AllowAll{})
	var typed *Error
	if !errors.As(err, &typed) || typed.Code != "invalid_params" {
		t.Fatalf("unknown field error = %v", err)
	}
	_, err = server.Dispatch(ctx, "nope", nil, AllowAll{})
	if !errors.As(err, &typed) || typed.Code != "unknown_method" {
		t.Fatalf("unknown method error = %v", err)
	}
}

func TestDispatchUnavailableSubsystems(t *testing.T) {
	server, _ := newTestServer(t)
	ctx := context.Background()
	for _, method := range []string{MethodRemoteStatus, MethodScreenStart} {
		_, err := server.Dispatch(ctx, method, json.RawMessage(`{"session_id":"s","view":true}`), AllowAll{})
		var typed *Error
		if !errors.As(err, &typed) || typed.Code != "unavailable" {
			t.Fatalf("%s error = %v", method, err)
		}
	}
}

func TestWorkspaceMirror(t *testing.T) {
	server, _ := newTestServer(t)
	ctx := context.Background()
	if _, err := server.Dispatch(ctx, MethodWorkspaceSet, json.RawMessage(`{"snapshot":{"spaces":[{"id":"x"}]}}`), AllowAll{}); err != nil {
		t.Fatal(err)
	}
	raw, err := server.Dispatch(ctx, MethodWorkspaceGet, nil, AllowAll{})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(raw.(json.RawMessage)), `"spaces"`) {
		t.Fatalf("workspace = %s", raw)
	}
	if _, err := server.Dispatch(ctx, MethodWorkspaceSet, json.RawMessage(`{"snapshot":"not-an-object"}`), AllowAll{}); err == nil {
		t.Fatal("non-object snapshot accepted")
	}
}

func TestClientAndSSE(t *testing.T) {
	server, _ := newTestServer(t)
	httpServer := httptest.NewServer(server.Handler("secret"))
	defer httpServer.Close()
	client := &Client{http: httpServer.Client(), base: httpServer.URL, token: "secret"}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if err := client.Health(ctx); err != nil {
		t.Fatal(err)
	}
	events, err := client.Events(ctx, "s1")
	if err != nil {
		t.Fatal(err)
	}
	server.Publish(Event{Type: EventUpdate, SessionID: "other", Error: "ignored"})
	server.Publish(Event{Type: EventUpdate, SessionID: "s1", Agent: &AgentSnapshot{Provider: "omp", State: "running"}})
	select {
	case event := <-events:
		if event.SessionID != "s1" || event.Agent == nil || event.Agent.State != "running" {
			t.Fatalf("event = %+v", event)
		}
	case <-ctx.Done():
		t.Fatal("no event received")
	}
}

func TestClientSurfacesTypedError(t *testing.T) {
	server, _ := newTestServer(t)
	httpServer := httptest.NewServer(server.Handler("secret"))
	defer httpServer.Close()
	client := &Client{http: httpServer.Client(), base: httpServer.URL, token: "secret"}
	_, err := client.AgentSnapshot(context.Background(), "missing")
	var typed *Error
	if !errors.As(err, &typed) || typed.Code != "not_found" {
		t.Fatalf("error = %v", err)
	}
}

func TestDeniedAuthorizer(t *testing.T) {
	server, _ := newTestServer(t)
	_, err := server.Dispatch(context.Background(), MethodAgentStart, json.RawMessage(`{"session_id":"s","cwd":"/tmp"}`), denyAll{})
	var typed *Error
	if !errors.As(err, &typed) || typed.Code != "forbidden" {
		t.Fatalf("error = %v", err)
	}
}

type denyAll struct{}

func (denyAll) Allow(context.Context, string, json.RawMessage) error {
	return &Denied{Reason: "no grant"}
}
