package remotescreen

import (
	"errors"
	"fmt"
	"image"
	"io"
	"sync"
	"time"
)

// PointerKind is the pointer transition to inject.
type PointerKind string

const (
	PointerMove PointerKind = "move"
	PointerDown PointerKind = "down"
	PointerUp   PointerKind = "up"
)

// Button identifies the mouse button used by PointerDown/PointerUp.
type Button string

const (
	ButtonNone   Button = ""
	ButtonLeft   Button = "left"
	ButtonRight  Button = "right"
	ButtonMiddle Button = "middle"
)

// KeyCode is a canonical, layout-independent key name (for example "KeyA",
// "Enter" or "ArrowLeft"). Platform adapters translate it to their native key
// code; unknown names are rejected before reaching the OS.
type KeyCode string

// Input injects pointer, keyboard and wheel events into the captured display.
// Implementations must be used behind GatedInput so every event is authorized
// and rate bounded.
type Input interface {
	Pointer(kind PointerKind, x, y int, button Button) error
	Key(code KeyCode, down bool) error
	Wheel(dx, dy int) error
}

var (
	// ErrInvalidInput reports a malformed pointer/key/wheel request.
	ErrInvalidInput = errors.New("remotescreen: invalid input event")
	// ErrPointOutOfBounds reports coordinates outside the captured display.
	ErrPointOutOfBounds = errors.New("remotescreen: pointer outside captured display")
	// ErrInputRateLimited reports an event rate above the safety bound.
	ErrInputRateLimited = errors.New("remotescreen: input rate exceeded")
	// ErrUnsupportedKey reports a key name the platform adapter cannot map.
	ErrUnsupportedKey = errors.New("remotescreen: unsupported key code")
	// ErrInputHelperMissing reports that the ukk-input helper binary is not
	// installed next to the app resources.
	ErrInputHelperMissing = errors.New("remotescreen: ukk-input helper not found")
	// ErrInputUnavailable reports a missing platform input backend.
	ErrInputUnavailable = errors.New("remotescreen: input injection unavailable")
	// ErrOSPermissionDenied reports that the OS has not granted the
	// Accessibility/input permission required for injection.
	ErrOSPermissionDenied = errors.New("remotescreen: OS input permission denied")
)

// Bounds limits event injection to the display that is being captured.
const (
	MaxInputEventsPerSecond = 500
	MaxWheelNotches         = 10
)

// GatedInput authorizes every event against screen.control and enforces
// coordinate and rate bounds before delegating to a platform adapter.
type GatedInput struct {
	auth   Authorizer
	inner  Input
	bounds image.Rectangle

	now func() time.Time

	mu          sync.Mutex
	windowStart time.Time
	events      int
}

// NewGatedInput wraps inner. bounds must be a non-empty display rectangle;
// points are validated as [Min,Max) in both axes.
func NewGatedInput(auth Authorizer, inner Input, bounds image.Rectangle) (*GatedInput, error) {
	if inner == nil {
		return nil, errors.New("remotescreen: nil input adapter")
	}
	if bounds.Empty() || bounds.Dx() <= 0 || bounds.Dy() <= 0 {
		return nil, fmt.Errorf("%w: empty display bounds %v", ErrInvalidInput, bounds)
	}
	return &GatedInput{
		auth:   auth,
		inner:  inner,
		bounds: bounds,
		now:    time.Now,
	}, nil
}

// Bounds returns the captured display rectangle used for validation.
func (g *GatedInput) Bounds() image.Rectangle { return g.bounds }

// Pointer implements Input.
func (g *GatedInput) Pointer(kind PointerKind, x, y int, button Button) error {
	switch kind {
	case PointerMove:
		if button != ButtonNone {
			return fmt.Errorf("%w: move with button %q", ErrInvalidInput, button)
		}
	case PointerDown, PointerUp:
		if button != ButtonLeft && button != ButtonRight && button != ButtonMiddle {
			return fmt.Errorf("%w: button %q", ErrInvalidInput, button)
		}
	default:
		return fmt.Errorf("%w: pointer kind %q", ErrInvalidInput, kind)
	}
	if err := g.check(); err != nil {
		return err
	}
	if !g.pointInBounds(x, y) {
		return fmt.Errorf("%w: (%d,%d) not in %v", ErrPointOutOfBounds, x, y, g.bounds)
	}
	if err := g.rateLimit(); err != nil {
		return err
	}
	return g.inner.Pointer(kind, x, y, button)
}

// Key implements Input.
func (g *GatedInput) Key(code KeyCode, down bool) error {
	if err := g.check(); err != nil {
		return err
	}
	if !validKeyCode(code) {
		return fmt.Errorf("%w: %q", ErrUnsupportedKey, code)
	}
	if err := g.rateLimit(); err != nil {
		return err
	}
	return g.inner.Key(code, down)
}

// Wheel implements Input. dx and dy are bounded notch counts; positive dy
// scrolls up and positive dx scrolls right on all adapters.
func (g *GatedInput) Wheel(dx, dy int) error {
	if dx == 0 && dy == 0 {
		return fmt.Errorf("%w: zero wheel delta", ErrInvalidInput)
	}
	if err := g.check(); err != nil {
		return err
	}
	if absInt(dx) > MaxWheelNotches || absInt(dy) > MaxWheelNotches {
		return fmt.Errorf("%w: wheel (%d,%d) exceeds %d notches", ErrInvalidInput, dx, dy, MaxWheelNotches)
	}
	if err := g.rateLimit(); err != nil {
		return err
	}
	return g.inner.Wheel(dx, dy)
}

// Close closes the platform adapter when it owns resources.
func (g *GatedInput) Close() error {
	if closer, ok := g.inner.(io.Closer); ok {
		return closer.Close()
	}
	return nil
}

func (g *GatedInput) pointInBounds(x, y int) bool {
	return x >= g.bounds.Min.X && x < g.bounds.Max.X &&
		y >= g.bounds.Min.Y && y < g.bounds.Max.Y
}

func (g *GatedInput) check() error {
	if g.auth == nil {
		return fmt.Errorf("%w: %s grant missing (no authorizer)", ErrNotAuthorized, ActionScreenControl)
	}
	return g.auth.Allow(ActionScreenControl)
}

func (g *GatedInput) rateLimit() error {
	g.mu.Lock()
	defer g.mu.Unlock()
	now := g.now()
	if g.windowStart.IsZero() || now.Sub(g.windowStart) >= time.Second {
		g.windowStart = now
		g.events = 0
	}
	if g.events >= MaxInputEventsPerSecond {
		return fmt.Errorf("%w: %d events/second", ErrInputRateLimited, MaxInputEventsPerSecond)
	}
	g.events++
	return nil
}

func absInt(v int) int {
	if v < 0 {
		return -v
	}
	return v
}

// OSPermission is optionally implemented by platform input adapters that can
// report a revocable OS permission (for example macOS Accessibility).
type OSPermission interface {
	OSPermission() error
}

// WireOSPermission installs the adapter's OS permission check on a
// GrantAuthorizer. Callers may also install their own probe.
func WireOSPermission(auth Authorizer, input Input) error {
	grant, ok := auth.(*GrantAuthorizer)
	if !ok {
		return nil
	}
	provider, ok := input.(OSPermission)
	if !ok {
		return nil
	}
	return grant.SetOSPermission(ActionScreenControl, provider.OSPermission)
}

// NewPlatformInput constructs the native input adapter for the current OS.
// It returns ErrInputHelperMissing or ErrInputUnavailable (both typed) when
// the runtime dependency is absent; callers must present that as an explicit
// state instead of silently accepting input.
func NewPlatformInput() (Input, error) {
	return newPlatformInput()
}
