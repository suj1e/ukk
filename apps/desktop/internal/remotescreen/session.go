package remotescreen

import (
	"context"
	"errors"
	"fmt"
	"io"
	"sync"
)

// ErrSessionActive reports a second capture start on one session.
var ErrSessionActive = errors.New("remotescreen: session already has an active capture")

// Session owns one capture lifecycle and the optional input adapter attached
// to it. Grants are checked at the boundary: Start requires screen.view,
// SetInput and every GatedInput event require screen.control. Stop tears down
// capture and input and clears all attachments so a revoked session cannot be
// revived by a stale reference.
type Session struct {
	auth Authorizer

	mu       sync.Mutex
	capture  Capture
	input    Input
	starting bool
	epoch    uint64
}

// NewSession returns a session bound to auth. A nil auth denies everything.
func NewSession(auth Authorizer) *Session {
	return &Session{auth: auth}
}

// Start authorizes screen.view, starts capture and attaches it.
func (s *Session) Start(ctx context.Context, capture Capture, opts Options) error {
	if capture == nil {
		return errors.New("remotescreen: nil capture")
	}
	if err := s.allow(ActionScreenView); err != nil {
		return err
	}

	s.mu.Lock()
	if s.capture != nil || s.starting {
		s.mu.Unlock()
		return ErrSessionActive
	}
	s.starting = true
	epoch := s.epoch
	s.mu.Unlock()

	err := capture.Start(ctx, opts)

	s.mu.Lock()
	s.starting = false
	if err != nil {
		s.mu.Unlock()
		return err
	}
	// A concurrent Stop bumped the epoch; tear the new capture down instead
	// of leaving it unowned.
	if s.capture != nil || s.epoch != epoch {
		s.mu.Unlock()
		_ = capture.Stop()
		return ErrSessionActive
	}
	s.capture = capture
	s.mu.Unlock()
	return nil
}

// SetInput attaches an input adapter after checking screen.control. The
// adapter itself is still validated per event by GatedInput.
func (s *Session) SetInput(input Input) error {
	if input == nil {
		return errors.New("remotescreen: nil input")
	}
	if err := s.allow(ActionScreenControl); err != nil {
		return err
	}
	s.mu.Lock()
	previous := s.input
	s.input = input
	s.mu.Unlock()
	if previous != nil && previous != input {
		if closer, ok := previous.(io.Closer); ok {
			if err := closer.Close(); err != nil {
				return fmt.Errorf("remotescreen: close replaced input: %w", err)
			}
		}
	}
	return nil
}

// Input returns the attached input adapter, or nil.
func (s *Session) Input() Input {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.input
}

// Active reports whether a capture is attached.
func (s *Session) Active() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.capture != nil
}

// Stop closes input, stops capture and clears session state. It is safe to
// call multiple times.
func (s *Session) Stop() error {
	s.mu.Lock()
	capture := s.capture
	input := s.input
	s.capture = nil
	s.input = nil
	s.epoch++
	s.mu.Unlock()

	var errs []error
	if closer, ok := input.(io.Closer); ok {
		if err := closer.Close(); err != nil {
			errs = append(errs, fmt.Errorf("remotescreen: close input: %w", err))
		}
	}
	if capture != nil {
		if err := capture.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("remotescreen: stop capture: %w", err))
		}
	}
	return errors.Join(errs...)
}

func (s *Session) allow(action string) error {
	if s.auth == nil {
		return fmt.Errorf("%w: %s grant missing (no authorizer)", ErrNotAuthorized, action)
	}
	return s.auth.Allow(action)
}
