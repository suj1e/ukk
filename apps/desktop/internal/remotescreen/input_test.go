package remotescreen

import (
	"errors"
	"image"
	"testing"
	"time"
)

type pointerCall struct {
	kind   PointerKind
	x, y   int
	button Button
}

type recordingInput struct {
	pointers []pointerCall
	keys     []KeyCode
	keyDown  []bool
	wheels   [][2]int
}

func (r *recordingInput) Pointer(kind PointerKind, x, y int, button Button) error {
	r.pointers = append(r.pointers, pointerCall{kind: kind, x: x, y: y, button: button})
	return nil
}

func (r *recordingInput) Key(code KeyCode, down bool) error {
	r.keys = append(r.keys, code)
	r.keyDown = append(r.keyDown, down)
	return nil
}

func (r *recordingInput) Wheel(dx, dy int) error {
	r.wheels = append(r.wheels, [2]int{dx, dy})
	return nil
}

type permInput struct {
	recordingInput
	err error
}

func (p *permInput) OSPermission() error { return p.err }

func newTestGated(t *testing.T, granted bool) (*GatedInput, *recordingInput, *GrantAuthorizer) {
	t.Helper()
	auth := NewGrantAuthorizer()
	if granted {
		if err := auth.Grant(ActionScreenControl); err != nil {
			t.Fatal(err)
		}
	}
	inner := &recordingInput{}
	gated, err := NewGatedInput(auth, inner, image.Rect(0, 0, 100, 50))
	if err != nil {
		t.Fatal(err)
	}
	return gated, inner, auth
}

func TestGatedInputDeniesWithoutControl(t *testing.T) {
	gated, inner, _ := newTestGated(t, false)
	if err := gated.Pointer(PointerMove, 10, 20, ButtonNone); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Pointer = %v", err)
	}
	if err := gated.Key(KeyCode("KeyA"), true); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Key = %v", err)
	}
	if err := gated.Wheel(0, 1); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Wheel = %v", err)
	}
	if len(inner.pointers)+len(inner.keys)+len(inner.wheels) != 0 {
		t.Fatalf("inner adapter received unauthorized events: %+v", inner)
	}
}

func TestGatedInputCoordinateBounds(t *testing.T) {
	gated, inner, _ := newTestGated(t, true)
	bad := []struct{ x, y int }{
		{-1, 10},
		{10, -1},
		{100, 10},
		{10, 50},
	}
	for _, point := range bad {
		err := gated.Pointer(PointerMove, point.x, point.y, ButtonNone)
		if !errors.Is(err, ErrPointOutOfBounds) {
			t.Fatalf("Pointer(%d,%d) = %v, want ErrPointOutOfBounds", point.x, point.y, err)
		}
	}
	if len(inner.pointers) != 0 {
		t.Fatalf("out-of-bounds pointer reached adapter: %+v", inner.pointers)
	}

	if err := gated.Pointer(PointerMove, 0, 0, ButtonNone); err != nil {
		t.Fatalf("minimum point: %v", err)
	}
	if err := gated.Pointer(PointerDown, 99, 49, ButtonLeft); err != nil {
		t.Fatalf("maximum point: %v", err)
	}
	if err := gated.Pointer(PointerUp, 99, 49, ButtonRight); err != nil {
		t.Fatalf("maximum point up: %v", err)
	}
	if len(inner.pointers) != 3 {
		t.Fatalf("adapter calls = %d", len(inner.pointers))
	}
}

func TestGatedInputPointerShapes(t *testing.T) {
	gated, inner, _ := newTestGated(t, true)
	if err := gated.Pointer(PointerMove, 1, 1, ButtonLeft); !errors.Is(err, ErrInvalidInput) {
		t.Fatalf("move with button = %v", err)
	}
	if err := gated.Pointer(PointerDown, 1, 1, ButtonNone); !errors.Is(err, ErrInvalidInput) {
		t.Fatalf("down without button = %v", err)
	}
	if err := gated.Pointer(PointerKind("drag"), 1, 1, ButtonLeft); !errors.Is(err, ErrInvalidInput) {
		t.Fatalf("unknown kind = %v", err)
	}
	if len(inner.pointers) != 0 {
		t.Fatal("invalid pointer reached adapter")
	}
}

func TestGatedInputKeyValidation(t *testing.T) {
	gated, inner, _ := newTestGated(t, true)
	if err := gated.Key(KeyCode("KeyA; rm -rf /"), true); !errors.Is(err, ErrUnsupportedKey) {
		t.Fatalf("invalid key = %v", err)
	}
	if err := gated.Key(KeyCode("KeyA"), true); err != nil {
		t.Fatalf("KeyA: %v", err)
	}
	if err := gated.Key(KeyEnter, false); err != nil {
		t.Fatalf("Enter: %v", err)
	}
	if len(inner.keys) != 2 || inner.keys[0] != KeyCode("KeyA") || inner.keyDown[1] {
		t.Fatalf("key calls = %v down=%v", inner.keys, inner.keyDown)
	}
}

func TestGatedInputWheelBounds(t *testing.T) {
	gated, inner, _ := newTestGated(t, true)
	if err := gated.Wheel(0, 0); !errors.Is(err, ErrInvalidInput) {
		t.Fatalf("zero wheel = %v", err)
	}
	if err := gated.Wheel(MaxWheelNotches+1, 0); !errors.Is(err, ErrInvalidInput) {
		t.Fatalf("oversized wheel = %v", err)
	}
	if err := gated.Wheel(-MaxWheelNotches, MaxWheelNotches); err != nil {
		t.Fatalf("bounded wheel: %v", err)
	}
	if len(inner.wheels) != 1 || inner.wheels[0] != [2]int{-MaxWheelNotches, MaxWheelNotches} {
		t.Fatalf("wheel calls = %v", inner.wheels)
	}
}

func TestGatedInputRateLimit(t *testing.T) {
	gated, inner, _ := newTestGated(t, true)
	now := time.Unix(1_000, 0)
	gated.now = func() time.Time { return now }

	for i := range MaxInputEventsPerSecond {
		if err := gated.Key(KeyCode("KeyA"), true); err != nil {
			t.Fatalf("event %d: %v", i, err)
		}
	}
	if err := gated.Key(KeyCode("KeyA"), false); !errors.Is(err, ErrInputRateLimited) {
		t.Fatalf("event over limit = %v", err)
	}
	if len(inner.keys) != MaxInputEventsPerSecond {
		t.Fatalf("adapter calls = %d", len(inner.keys))
	}

	now = now.Add(time.Second)
	if err := gated.Key(KeyCode("KeyA"), false); err != nil {
		t.Fatalf("event after window: %v", err)
	}
}

func TestGatedInputRequiresNonEmptyBounds(t *testing.T) {
	auth := NewGrantAuthorizer()
	if _, err := NewGatedInput(auth, &recordingInput{}, image.Rect(0, 0, 0, 0)); !errors.Is(err, ErrInvalidInput) {
		t.Fatalf("empty bounds = %v", err)
	}
	if _, err := NewGatedInput(auth, nil, image.Rect(0, 0, 1, 1)); err == nil {
		t.Fatal("nil input accepted")
	}
}

func TestWireOSPermission(t *testing.T) {
	auth := NewGrantAuthorizer()
	if err := auth.Grant(ActionScreenControl); err != nil {
		t.Fatal(err)
	}
	input := &permInput{err: ErrOSPermissionDenied}
	if err := WireOSPermission(auth, input); err != nil {
		t.Fatal(err)
	}
	if err := auth.Allow(ActionScreenControl); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Allow with denied OS permission = %v", err)
	}

	input.err = nil
	if err := auth.Allow(ActionScreenControl); err != nil {
		t.Fatalf("Allow with granted OS permission = %v", err)
	}
}
