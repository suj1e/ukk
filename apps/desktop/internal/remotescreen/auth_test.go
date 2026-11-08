package remotescreen

import (
	"context"
	"errors"
	"io"
	"testing"
)

type fakeCapture struct {
	started  bool
	stopped  bool
	opts     Options
	frames   chan Frame
	startErr error
}

func (f *fakeCapture) Start(context.Context, Options) error {
	if f.startErr != nil {
		return f.startErr
	}
	f.started = true
	return nil
}

func (f *fakeCapture) Frames() <-chan Frame {
	if f.frames == nil {
		f.frames = make(chan Frame)
	}
	return f.frames
}

func (f *fakeCapture) Stop() error {
	f.stopped = true
	return nil
}

func TestGrantAuthorizerDeniesByDefault(t *testing.T) {
	a := NewGrantAuthorizer()
	if err := a.Allow(ActionScreenView); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("view = %v, want ErrNotAuthorized", err)
	}
	if err := a.Allow(ActionScreenControl); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("control = %v, want ErrNotAuthorized", err)
	}

	if err := a.Grant(ActionScreenView); err != nil {
		t.Fatal(err)
	}
	if err := a.Allow(ActionScreenView); err != nil {
		t.Fatalf("granted view: %v", err)
	}
	if err := a.Allow(ActionScreenControl); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("control without grant = %v", err)
	}

	if err := a.Revoke(ActionScreenView); err != nil {
		t.Fatal(err)
	}
	if err := a.Allow(ActionScreenView); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("revoked view = %v", err)
	}
}

func TestGrantAuthorizerUnknownAction(t *testing.T) {
	a := NewGrantAuthorizer()
	if err := a.Grant("agent.control"); !errors.Is(err, ErrUnknownAction) {
		t.Fatalf("Grant = %v, want ErrUnknownAction", err)
	}
	if err := a.Allow("pairing"); !errors.Is(err, ErrUnknownAction) {
		t.Fatalf("Allow = %v, want ErrUnknownAction", err)
	}
}

func TestGrantAuthorizerOSPermission(t *testing.T) {
	a := NewGrantAuthorizer()
	if err := a.Grant(ActionScreenControl); err != nil {
		t.Fatal(err)
	}
	denied := errors.New("accessibility off")
	if err := a.SetOSPermission(ActionScreenControl, func() error { return denied }); err != nil {
		t.Fatal(err)
	}
	err := a.Allow(ActionScreenControl)
	if !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Allow = %v, want ErrNotAuthorized", err)
	}

	if err := a.SetOSPermission(ActionScreenControl, func() error { return nil }); err != nil {
		t.Fatal(err)
	}
	if err := a.Allow(ActionScreenControl); err != nil {
		t.Fatalf("Allow with permission = %v", err)
	}
}

func TestSessionDeniesCaptureWithoutView(t *testing.T) {
	auth := NewGrantAuthorizer()
	session := NewSession(auth)
	capture := &fakeCapture{}

	err := session.Start(context.Background(), capture, Options{})
	if !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Start = %v, want ErrNotAuthorized", err)
	}
	if capture.started {
		t.Fatal("capture started without screen.view")
	}
	if session.Active() {
		t.Fatal("session active after denied start")
	}

	if err := auth.Grant(ActionScreenView); err != nil {
		t.Fatal(err)
	}
	if err := session.Start(context.Background(), capture, Options{FPS: 10}); err != nil {
		t.Fatalf("Start with view: %v", err)
	}
	if !capture.started || !session.Active() {
		t.Fatal("capture was not attached")
	}
	if err := session.Start(context.Background(), &fakeCapture{}, Options{}); !errors.Is(err, ErrSessionActive) {
		t.Fatalf("second Start = %v, want ErrSessionActive", err)
	}

	if err := session.Stop(); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	if !capture.stopped {
		t.Fatal("capture was not stopped")
	}
	if session.Active() {
		t.Fatal("session still active after Stop")
	}
	if err := session.Stop(); err != nil {
		t.Fatalf("second Stop: %v", err)
	}
}

func TestSessionRequiresControlForInput(t *testing.T) {
	auth := NewGrantAuthorizer()
	session := NewSession(auth)
	input := &fakeInputCloser{}

	if err := session.SetInput(input); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("SetInput = %v, want ErrNotAuthorized", err)
	}
	if session.Input() != nil {
		t.Fatal("input attached without screen.control")
	}

	if err := auth.Grant(ActionScreenControl); err != nil {
		t.Fatal(err)
	}
	if err := session.SetInput(input); err != nil {
		t.Fatalf("SetInput with control: %v", err)
	}
	if session.Input() != input {
		t.Fatal("input not attached")
	}
	if err := session.Stop(); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	if !input.closed {
		t.Fatal("input was not closed on Stop")
	}
	if session.Input() != nil {
		t.Fatal("input state not cleared")
	}
}

func TestSessionNilAuthorizerDenies(t *testing.T) {
	session := NewSession(nil)
	err := session.Start(context.Background(), &fakeCapture{}, Options{})
	if !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Start = %v, want ErrNotAuthorized", err)
	}
}

type fakeInputCloser struct {
	closed bool
}

func (f *fakeInputCloser) Pointer(PointerKind, int, int, Button) error { return nil }
func (f *fakeInputCloser) Key(KeyCode, bool) error                     { return nil }
func (f *fakeInputCloser) Wheel(int, int) error                        { return nil }
func (f *fakeInputCloser) Close() error {
	f.closed = true
	return nil
}

var (
	_ io.Closer = (*fakeInputCloser)(nil)
	_ Capture   = (*fakeCapture)(nil)
)
