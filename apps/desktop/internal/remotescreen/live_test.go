package remotescreen

import (
	"bytes"
	"context"
	"os"
	"testing"
	"time"
)

// TestFFmpegLiveCapture is opt-in because it captures the real display and
// requires macOS Screen Recording permission. CI skips it.
func TestFFmpegLiveCapture(t *testing.T) {
	if os.Getenv("UKK_SCREEN_LIVE") != "1" {
		t.Skip("set UKK_SCREEN_LIVE=1 to capture the real display")
	}
	capture := NewFFmpegCapture()
	if err := capture.Available(); err != nil {
		t.Skipf("ffmpeg unavailable: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := capture.Start(ctx, Options{FPS: 5, MaxWidth: 640, Bitrate: 1_000_000}); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer capture.Stop()

	var frame Frame
	select {
	case got, ok := <-capture.Frames():
		if !ok {
			t.Fatalf("frame channel closed before a frame: %v (%s)", capture.LastError(), capture.Diagnostics())
		}
		frame = got
	case <-time.After(20 * time.Second):
		t.Fatalf("no frame within 20s: %v (%s)", capture.LastError(), capture.Diagnostics())
	}

	if len(frame.Data) == 0 {
		t.Fatal("empty frame")
	}
	if !bytes.HasPrefix(frame.Data, []byte{0, 0, 0, 1}) {
		t.Fatalf("frame does not start with Annex-B four-byte code: %x", frame.Data[:min(8, len(frame.Data))])
	}
	if !frame.Keyframe && !annexBContainsIDR(frame.Data) {
		t.Fatalf("first frame has no IDR (keyframe=%v)", frame.Keyframe)
	}

	if err := capture.Stop(); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		select {
		case _, ok := <-capture.Frames():
			if !ok {
				return
			}
		case <-time.After(time.Until(deadline)):
			t.Fatal("frame channel did not close after Stop")
		}
	}
}
