package remotescreen

import (
	"context"
	"errors"
	"fmt"
	"sync"
)

// ErrStreamActive reports a second capture start on one stream.
var ErrStreamActive = errors.New("remotescreen: stream already active")

// Stream wires an authorized capture to a media Publisher. It is the media
// pipeline used by Remote Screen after the user grants screen.view; input
// authorization stays on GatedInput/screen.control.
//
// Closing the stream stops capture, waits for the frame pump and closes the
// publisher, so a revoked or dismissed view cannot keep capturing.
type Stream struct {
	auth      Authorizer
	publisher *Publisher

	mu      sync.Mutex
	capture Capture
	done    chan struct{}
	started bool
	closed  bool

	errMu   sync.Mutex
	lastErr error
}

// NewStream binds auth and publisher. A nil authorizer denies capture.
func NewStream(auth Authorizer, publisher *Publisher) (*Stream, error) {
	if publisher == nil {
		return nil, errors.New("remotescreen: nil publisher")
	}
	return &Stream{auth: auth, publisher: publisher}, nil
}

// Start authorizes screen.view, starts capture and pumps access units into the
// publisher. The publisher must already have produced an offer; otherwise the
// pump reports ErrPublisherNotReady and stops capture.
func (s *Stream) Start(ctx context.Context, capture Capture, opts Options) error {
	if capture == nil {
		return errors.New("remotescreen: nil capture")
	}
	if s.auth == nil {
		return fmt.Errorf("%w: %s grant missing (no authorizer)", ErrNotAuthorized, ActionScreenView)
	}
	if err := s.auth.Allow(ActionScreenView); err != nil {
		return err
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return ErrPublisherClosed
	}
	if s.started {
		return ErrStreamActive
	}
	s.started = true
	s.done = make(chan struct{})
	if err := capture.Start(ctx, opts); err != nil {
		s.started = false
		close(s.done)
		s.done = nil
		return err
	}
	if err := s.publisher.AttachCapture(capture); err != nil {
		_ = capture.Stop()
		s.started = false
		close(s.done)
		s.done = nil
		return err
	}
	s.capture = capture
	go s.pump(capture.Frames(), s.done)
	return nil
}

func (s *Stream) pump(frames <-chan Frame, done chan struct{}) {
	defer close(done)
	for {
		select {
		case frame, ok := <-frames:
			if !ok {
				return
			}
			if err := s.publisher.WriteFrame(frame); err != nil {
				s.setError(err)
				s.mu.Lock()
				capture := s.capture
				s.mu.Unlock()
				if capture != nil {
					_ = capture.Stop()
				}
				return
			}
		case <-done:
			return
		}
	}
}

// LastError returns the last frame-pump error.
func (s *Stream) LastError() error {
	s.errMu.Lock()
	defer s.errMu.Unlock()
	return s.lastErr
}

func (s *Stream) setError(err error) {
	s.errMu.Lock()
	s.lastErr = err
	s.errMu.Unlock()
}

// Close stops capture, waits for the pump and closes media resources. It is
// idempotent and clears the capture attachment.
func (s *Stream) Close() error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil
	}
	s.closed = true
	capture := s.capture
	s.capture = nil
	done := s.done
	s.mu.Unlock()

	var errs []error
	if capture != nil {
		if err := capture.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("remotescreen: stop capture: %w", err))
		}
	}
	if done != nil {
		<-done
	}
	if err := s.publisher.Close(); err != nil {
		errs = append(errs, fmt.Errorf("remotescreen: close publisher: %w", err))
	}
	return errors.Join(errs...)
}

// Stop is an alias for Close.
func (s *Stream) Stop() error { return s.Close() }
