// Package remotescreen implements the first-party Remote Screen media and
// input adapters. Capture and input injection are separate authorities: a
// screen.view grant is required before capture starts and every input event
// requires a screen.control grant. The relay never carries frames produced by
// this package.
package remotescreen

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// Capture is a platform screen-capture source. Frames delivers encoded
// Annex-B H.264 access units. Frames is closed after Stop or after the
// underlying encoder exits.
type Capture interface {
	Start(ctx context.Context, opts Options) error
	Frames() <-chan Frame
	Stop() error
}

// Options controls a capture session.
//
// Bitrate is expressed in bits per second. Zero values select documented
// defaults; negative or out-of-range values are rejected rather than silently
// clamped.
type Options struct {
	DisplayID string
	FPS       int
	MaxWidth  int
	Bitrate   int
}

// Frame is one encoded access unit.
//
// Data is Annex-B H.264: every NAL unit is prefixed with a four-byte start
// code. PTS is monotonic from the start of the capture. Width and Height are
// zero unless the capture adapter can determine them from the bitstream.
type Frame struct {
	Data     []byte
	PTS      time.Duration
	Keyframe bool
	Width    int
	Height   int
}

// ErrCaptureRunning is returned when Start is called on a live capture.
var ErrCaptureRunning = errors.New("remotescreen: capture is already running")

// ErrFFmpegUnavailable reports that the ffmpeg executable could not be found.
// It is a normal, explicit state: callers should present capture as
// unavailable instead of starting a process.
var ErrFFmpegUnavailable = errors.New("remotescreen: ffmpeg executable not found")

// ErrFrameTooLarge reports an encoded access unit or pending Annex-B buffer
// that exceeds the configured bound.
var ErrFrameTooLarge = errors.New("remotescreen: encoded frame exceeds bound")

// ErrInvalidOptions reports an invalid capture option.
var ErrInvalidOptions = errors.New("remotescreen: invalid capture options")

// OptionsError names the rejected field.
type OptionsError struct {
	Field  string
	Reason string
}

func (e *OptionsError) Error() string {
	return fmt.Sprintf("%s: %s=%s", ErrInvalidOptions, e.Field, e.Reason)
}

// Unwrap exposes ErrInvalidOptions to errors.Is.
func (e *OptionsError) Unwrap() error { return ErrInvalidOptions }

// UnsupportedPlatformError reports that no capture adapter exists for a GOOS.
type UnsupportedPlatformError struct {
	Platform string
}

func (e *UnsupportedPlatformError) Error() string {
	return fmt.Sprintf("remotescreen: screen capture is unsupported on %s", e.Platform)
}

// UnsupportedDisplayError reports a display identifier that the selected
// adapter cannot address.
type UnsupportedDisplayError struct {
	Platform  string
	DisplayID string
}

func (e *UnsupportedDisplayError) Error() string {
	return fmt.Sprintf("remotescreen: display %q is not supported on %s", e.DisplayID, e.Platform)
}
