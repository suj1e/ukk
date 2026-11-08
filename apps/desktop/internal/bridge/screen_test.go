package bridge

import (
	"testing"

	"github.com/suj1e/ukk/apps/desktop/internal/remote"
	"github.com/suj1e/ukk/apps/desktop/internal/remotescreen"
)

type recordingInput struct {
	pointers []string
	keys     []string
	wheels   [][2]int
}

func (r *recordingInput) Pointer(kind remotescreen.PointerKind, x, y int, button remotescreen.Button) error {
	r.pointers = append(r.pointers, string(kind)+":"+string(button))
	return nil
}

func (r *recordingInput) Key(code remotescreen.KeyCode, down bool) error {
	r.keys = append(r.keys, string(code))
	return nil
}

func (r *recordingInput) Wheel(dx, dy int) error {
	r.wheels = append(r.wheels, [2]int{dx, dy})
	return nil
}

func TestInjectMapsEvents(t *testing.T) {
	recorder := &recordingInput{}
	cases := []InputEvent{
		{Kind: "pointer", X: 10, Y: 20},
		{Kind: "down", X: 10, Y: 20, Button: 2},
		{Kind: "up", X: 10, Y: 20, Button: 3},
		{Kind: "wheel", DeltaX: -1, DeltaY: 2},
		{Kind: "key", Code: "Enter", Down: true},
	}
	for _, event := range cases {
		if err := inject(recorder, event); err != nil {
			t.Fatalf("inject %+v: %v", event, err)
		}
	}
	if len(recorder.pointers) != 3 {
		t.Fatalf("pointers = %+v", recorder.pointers)
	}
	if recorder.pointers[1] != "down:right" || recorder.pointers[2] != "up:middle" {
		t.Fatalf("pointer mapping = %+v", recorder.pointers)
	}
	if len(recorder.wheels) != 1 || recorder.wheels[0] != [2]int{-1, 2} {
		t.Fatalf("wheels = %+v", recorder.wheels)
	}
	if len(recorder.keys) != 1 || recorder.keys[0] != "Enter" {
		t.Fatalf("keys = %+v", recorder.keys)
	}
	if err := inject(recorder, InputEvent{Kind: "key"}); err == nil {
		t.Fatal("empty key code accepted")
	}
	if err := inject(recorder, InputEvent{Kind: "bogus"}); err == nil {
		t.Fatal("unknown event accepted")
	}
}

func TestScopeMapping(t *testing.T) {
	cases := map[string]remote.Scope{
		MethodAgentSnapshot: remote.ScopeSessionObserve,
		MethodAgentPrompt:   remote.ScopeAgentSend,
		MethodAgentStop:     remote.ScopeAgentSend,
		MethodAgentApprove:  remote.ScopeAgentApprove,
		MethodRepoFile:      remote.ScopeFilesystemRead,
		MethodScreenStart:   remote.ScopeScreenView,
		MethodScreenInput:   remote.ScopeScreenControl,
		MethodWorkspaceGet:  remote.ScopeWorkspaceRead,
	}
	for method, want := range cases {
		got, ok := scopeForMethod(method)
		if !ok || got != want {
			t.Fatalf("%s scope = %q, %v; want %q", method, got, ok, want)
		}
	}
	for _, method := range []string{MethodRemoteGrant, MethodRemoteRevoke, MethodRemoteConfirm, "unknown"} {
		if _, ok := scopeForMethod(method); ok {
			t.Fatalf("%s should not be available to a paired device", method)
		}
	}
}
