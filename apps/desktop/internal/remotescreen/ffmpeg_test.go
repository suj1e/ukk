package remotescreen

import (
	"context"
	"errors"
	"os/exec"
	"strings"
	"testing"
)

func TestFFmpegArgsPerOS(t *testing.T) {
	opts := Options{FPS: 24, MaxWidth: 1280, Bitrate: 3_000_000}

	tests := []struct {
		name    string
		goos    string
		display string
		prefix  []string
	}{
		{
			name:    "darwin",
			goos:    "darwin",
			display: "Capture screen 2",
			prefix: []string{
				"-hide_banner", "-nostdin",
				"-f", "avfoundation", "-capture_cursor", "1",
				"-framerate", "30", "-i", "Capture screen 2",
			},
		},
		{
			name:   "windows",
			goos:   "windows",
			prefix: []string{"-hide_banner", "-nostdin", "-f", "gdigrab", "-i", "desktop"},
		},
		{
			name:    "linux",
			goos:    "linux",
			display: ":1.0",
			prefix:  []string{"-hide_banner", "-nostdin", "-f", "x11grab", "-i", ":1.0"},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			opts := opts
			opts.DisplayID = tc.display
			args, err := ffmpegArgs(tc.goos, opts)
			if err != nil {
				t.Fatalf("ffmpegArgs: %v", err)
			}
			assertSequence(t, args, tc.prefix)
			assertSequence(t, args, []string{
				"-an", "-c:v", "libx264", "-profile:v", "baseline",
				"-preset", "ultrafast", "-tune", "zerolatency",
				"-pix_fmt", "yuv420p",
			})
			assertSequence(t, args, []string{"-x264-params", "repeat-headers=1"})
			assertSequence(t, args, []string{"-b:v", "3000000"})
			assertSequence(t, args, []string{
				"-vf", "fps=24,scale=w='min(1280,iw)':h=-2:flags=bilinear",
			})
			assertSequence(t, args, []string{"-f", "h264", "pipe:1"})
			if got := args[len(args)-1]; got != "pipe:1" {
				t.Fatalf("last arg = %q, want pipe:1", got)
			}
		})
	}
}

func TestFFmpegArgsDefaultsAndDisplayDefaults(t *testing.T) {
	args, err := ffmpegArgs("darwin", Options{})
	if err != nil {
		t.Fatal(err)
	}
	assertSequence(t, args, []string{
		"-hide_banner", "-nostdin",
		"-f", "avfoundation", "-capture_cursor", "1", "-framerate", "30", "-i", "1",
	})
	assertSequence(t, args, []string{"-g", "30", "-keyint_min", "15"})
	assertSequence(t, args, []string{"-b:v", "2500000"})
	assertSequence(t, args, []string{"-vf", "fps=15"})
	for _, arg := range args {
		if strings.Contains(arg, "scale=") {
			t.Fatalf("unexpected scale filter: %q", arg)
		}
	}
}

func TestFFmpegArgsLinuxDisplayFromEnv(t *testing.T) {
	t.Setenv("DISPLAY", ":42.0")
	args, err := ffmpegArgs("linux", Options{})
	if err != nil {
		t.Fatal(err)
	}
	assertSequence(t, args, []string{"-i", ":42.0"})
}

func TestFFmpegArgsUnsupportedPlatform(t *testing.T) {
	_, err := ffmpegArgs("plan9", Options{})
	var platformErr *UnsupportedPlatformError
	if !errors.As(err, &platformErr) {
		t.Fatalf("err = %v, want UnsupportedPlatformError", err)
	}
	if platformErr.Platform != "plan9" {
		t.Fatalf("platform = %q", platformErr.Platform)
	}
}

func TestFFmpegArgsUnsupportedDisplay(t *testing.T) {
	tests := []struct {
		goos    string
		display string
	}{
		{goos: "darwin", display: "screen 0"},
		{goos: "windows", display: "Some Window"},
		{goos: "linux", display: "not-a-display"},
	}
	for _, tc := range tests {
		t.Run(tc.goos, func(t *testing.T) {
			_, err := ffmpegArgs(tc.goos, Options{DisplayID: tc.display})
			var displayErr *UnsupportedDisplayError
			if !errors.As(err, &displayErr) {
				t.Fatalf("err = %v, want UnsupportedDisplayError", err)
			}
			if displayErr.DisplayID != tc.display {
				t.Fatalf("display = %q", displayErr.DisplayID)
			}
		})
	}
}

func TestFFmpegArgsRejectInvalidOptions(t *testing.T) {
	tests := []Options{
		{FPS: 61},
		{FPS: -1},
		{Bitrate: minBitrate - 1},
		{Bitrate: maxBitrate + 1},
		{MaxWidth: maxCaptureWidth + 1},
	}
	for _, opts := range tests {
		_, err := ffmpegArgs("darwin", opts)
		if !errors.Is(err, ErrInvalidOptions) {
			t.Fatalf("opts %+v: err = %v, want ErrInvalidOptions", opts, err)
		}
	}
}

func TestFFmpegCaptureUnavailableDoesNotExecute(t *testing.T) {
	c := NewFFmpegCapture()
	c.lookPath = func(string) (string, error) { return "", exec.ErrNotFound }

	if err := c.Available(); !errors.Is(err, ErrFFmpegUnavailable) {
		t.Fatalf("Available = %v, want ErrFFmpegUnavailable", err)
	}
	err := c.Start(context.Background(), Options{})
	if !errors.Is(err, ErrFFmpegUnavailable) {
		t.Fatalf("Start = %v, want ErrFFmpegUnavailable", err)
	}
	if err := c.Stop(); err != nil {
		t.Fatalf("Stop on idle capture = %v", err)
	}
}

func TestBoundedBufferKeepsTail(t *testing.T) {
	b := newBoundedBuffer(8)
	b.Write([]byte("0123456789"))
	if got := b.String(); got != "23456789" {
		t.Fatalf("buffer = %q", got)
	}
	b.Write([]byte("abc"))
	if got := b.String(); got != "56789abc" {
		t.Fatalf("buffer after append = %q", got)
	}
}

func assertSequence(t *testing.T, args, want []string) {
	t.Helper()
	if len(want) > len(args) {
		t.Fatalf("args %v shorter than wanted sequence %v", args, want)
	}
	for i := 0; i+len(want) <= len(args); i++ {
		match := true
		for j := range want {
			if args[i+j] != want[j] {
				match = false
				break
			}
		}
		if match {
			return
		}
	}
	t.Fatalf("args %v do not contain sequence %v", args, want)
}
