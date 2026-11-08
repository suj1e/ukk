package remotescreen

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"
)

type streamCapture struct {
	mu      sync.Mutex
	frames  chan Frame
	started bool
	stopped bool
	close   sync.Once
}

func newStreamCapture() *streamCapture {
	return &streamCapture{frames: make(chan Frame, 4)}
}

func (c *streamCapture) Start(context.Context, Options) error {
	c.mu.Lock()
	c.started = true
	c.mu.Unlock()
	return nil
}

func (c *streamCapture) Frames() <-chan Frame { return c.frames }

func (c *streamCapture) Stop() error {
	c.mu.Lock()
	c.stopped = true
	c.mu.Unlock()
	c.close.Do(func() { close(c.frames) })
	return nil
}

func (c *streamCapture) status() (started, stopped bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.started, c.stopped
}

func TestStreamRequiresViewAndPumpsFrames(t *testing.T) {
	auth := NewGrantAuthorizer()
	publisher, err := NewPublisher(PublisherOptions{STUNURLs: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	stream, err := NewStream(auth, publisher)
	if err != nil {
		t.Fatal(err)
	}
	capture := newStreamCapture()

	if err := stream.Start(context.Background(), capture, Options{}); !errors.Is(err, ErrNotAuthorized) {
		t.Fatalf("Start without view = %v", err)
	}
	if started, _ := capture.status(); started {
		t.Fatal("capture started without screen.view")
	}

	if err := auth.Grant(ActionScreenView); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if _, err := publisher.CreateOffer(ctx); err != nil {
		t.Fatal(err)
	}
	if err := stream.Start(ctx, capture, Options{FPS: 5}); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if started, _ := capture.status(); !started {
		t.Fatal("capture not started")
	}
	if err := stream.Start(ctx, capture, Options{}); !errors.Is(err, ErrStreamActive) {
		t.Fatalf("second Start = %v", err)
	}

	capture.frames <- Frame{Data: sc4(nal(nalTypeIDRSlice, 0x80)), PTS: 0, Keyframe: true}
	deadline := time.Now().Add(2 * time.Second)
	for {
		if _, ok := publisher.LastKeyframePTS(); ok {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("frame was not pumped into publisher")
		}
		time.Sleep(5 * time.Millisecond)
	}

	if err := stream.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if stopped, _ := capture.status(); !stopped {
		t.Fatal("Close did not stop capture")
	}
	if err := publisher.WriteFrame(Frame{Data: sc4(nal(nalTypeIDRSlice, 0x80))}); !errors.Is(err, ErrPublisherClosed) {
		t.Fatalf("publisher after stream close = %v", err)
	}
	if err := stream.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
}
