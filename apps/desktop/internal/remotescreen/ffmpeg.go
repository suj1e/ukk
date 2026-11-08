package remotescreen

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"regexp"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const (
	defaultFPS        = 15
	defaultBitrate    = 2_500_000
	minBitrate        = 100_000
	maxBitrate        = 50_000_000
	maxFPS            = 60
	maxCaptureWidth   = 7680
	frameQueueDepth   = 8
	ffmpegReadChunk   = 64 << 10
	ffmpegStderrLimit = 8 << 10
	stopGracePeriod   = 2 * time.Second
)

var (
	darwinDisplayPattern = regexp.MustCompile(`^(?:[0-9]+|Capture screen [0-9]+)$`)
	linuxDisplayPattern  = regexp.MustCompile(`^(?:[A-Za-z0-9._-]*:[0-9]+(?:\.[0-9]+)?)$`)
)

func (o Options) normalized() (Options, error) {
	if o.FPS == 0 {
		o.FPS = defaultFPS
	}
	if o.FPS < 1 || o.FPS > maxFPS {
		return Options{}, &OptionsError{Field: "FPS", Reason: strconv.Itoa(o.FPS)}
	}
	if o.Bitrate == 0 {
		o.Bitrate = defaultBitrate
	}
	if o.Bitrate < minBitrate || o.Bitrate > maxBitrate {
		return Options{}, &OptionsError{Field: "Bitrate", Reason: strconv.Itoa(o.Bitrate)}
	}
	if o.MaxWidth < 0 || o.MaxWidth > maxCaptureWidth {
		return Options{}, &OptionsError{Field: "MaxWidth", Reason: strconv.Itoa(o.MaxWidth)}
	}
	return o, nil
}

// ffmpegArgs builds the ffmpeg command line for goos. It never executes
// anything; the returned args are passed directly to exec.Command, not to a
// shell. Display identifiers are validated per adapter before they reach
// ffmpeg.
func ffmpegArgs(goos string, opts Options) ([]string, error) {
	opts, err := opts.normalized()
	if err != nil {
		return nil, err
	}

	fps := strconv.Itoa(opts.FPS)
	// Global options: never read stdin and keep stderr free of the banner.
	args := []string{"-hide_banner", "-nostdin"}

	switch goos {
	case "darwin":
		display := opts.DisplayID
		if display == "" {
			display = "1"
		}
		if !darwinDisplayPattern.MatchString(display) {
			return nil, &UnsupportedDisplayError{Platform: goos, DisplayID: display}
		}
		// avfoundation screen devices require an exact capture mode. 30 fps
		// is the native screen rate on macOS; the fps filter below produces
		// the requested output rate without rejecting the device.
		args = append(args,
			"-f", "avfoundation",
			"-capture_cursor", "1",
			"-framerate", "30",
			"-i", display,
		)
	case "windows":
		display := opts.DisplayID
		if display == "" {
			display = "desktop"
		}
		if display != "desktop" {
			return nil, &UnsupportedDisplayError{Platform: goos, DisplayID: display}
		}
		args = append(args,
			"-f", "gdigrab",
			"-i", "desktop",
		)
	case "linux":
		display := opts.DisplayID
		if display == "" {
			display = os.Getenv("DISPLAY")
			if display == "" {
				display = ":0.0"
			}
		}
		if !linuxDisplayPattern.MatchString(display) {
			return nil, &UnsupportedDisplayError{Platform: goos, DisplayID: display}
		}
		args = append(args,
			"-f", "x11grab",
			"-i", display,
		)
	default:
		return nil, &UnsupportedPlatformError{Platform: goos}
	}

	args = append(args,
		"-an",
		"-c:v", "libx264",
		"-profile:v", "baseline",
		"-preset", "ultrafast",
		"-tune", "zerolatency",
		"-pix_fmt", "yuv420p",
		"-g", strconv.Itoa(opts.FPS*2),
		"-keyint_min", strconv.Itoa(opts.FPS),
		"-sc_threshold", "0",
		"-x264-params", "repeat-headers=1",
		"-b:v", strconv.Itoa(opts.Bitrate),
		"-maxrate", strconv.Itoa(opts.Bitrate),
		"-bufsize", strconv.Itoa(opts.Bitrate*2),
	)
	filters := []string{"fps=" + fps}
	if opts.MaxWidth > 0 {
		filters = append(filters,
			fmt.Sprintf("scale=w='min(%d,iw)':h=-2:flags=bilinear", opts.MaxWidth))
	}
	args = append(args, "-vf", strings.Join(filters, ","))
	args = append(args, "-f", "h264", "pipe:1")
	return args, nil
}

// FFmpegCapture captures a display with the external ffmpeg executable and
// emits Annex-B H.264 access units. It deliberately does not vendor or
// require ffmpeg: Availability reports ErrFFmpegUnavailable so the UI can
// present an explicit unavailable state.
type FFmpegCapture struct {
	mu       sync.Mutex
	lookPath func(string) (string, error)
	frames   chan Frame
	run      *ffmpegRun
	running  bool

	lastErr    error
	diagnostic string
}

type ffmpegRun struct {
	parent *FFmpegCapture
	cmd    *exec.Cmd
	stdout io.ReadCloser
	stderr io.ReadCloser
	frames chan Frame

	readDone   chan struct{}
	stderrDone chan struct{}
	done       chan struct{}
	stopCh     chan struct{}
	stopOnce   sync.Once
	stopping   atomic.Bool

	parser    *AnnexBParser
	stderrBuf *boundedBuffer
	errMu     sync.Mutex
	lastErr   error
	fps       int
	ptsIndex  int
}

// NewFFmpegCapture returns an idle capture adapter.
func NewFFmpegCapture() *FFmpegCapture {
	return &FFmpegCapture{
		lookPath: exec.LookPath,
		frames:   make(chan Frame, frameQueueDepth),
	}
}

// Start launches ffmpeg and starts the bounded reader loops. The supplied
// context cancels the capture; use Stop for the normal path.
func (c *FFmpegCapture) Start(ctx context.Context, opts Options) error {
	if ctx == nil {
		return errors.New("remotescreen: nil capture context")
	}
	opts, err := opts.normalized()
	if err != nil {
		return err
	}
	goos := runtime.GOOS
	args, err := ffmpegArgs(goos, opts)
	if err != nil {
		return err
	}
	path, err := c.lookPath("ffmpeg")
	if err != nil {
		return fmt.Errorf("%w: %v", ErrFFmpegUnavailable, err)
	}

	c.mu.Lock()
	defer c.mu.Unlock()
	if c.running {
		return ErrCaptureRunning
	}

	cmd := exec.Command(path, args...)
	// No shell, no interpolation and no stdin: args are an argv slice.
	cmd.Stdin = nil
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("remotescreen: ffmpeg stdout: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("remotescreen: ffmpeg stderr: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("remotescreen: start ffmpeg: %w", err)
	}

	frames := make(chan Frame, frameQueueDepth)
	run := &ffmpegRun{
		parent:     c,
		cmd:        cmd,
		stdout:     stdout,
		stderr:     stderr,
		frames:     frames,
		readDone:   make(chan struct{}),
		stderrDone: make(chan struct{}),
		done:       make(chan struct{}),
		stopCh:     make(chan struct{}),
		parser:     NewAnnexBParser(),
		stderrBuf:  newBoundedBuffer(ffmpegStderrLimit),
		fps:        opts.FPS,
	}
	c.run = run
	c.running = true
	c.frames = frames
	c.lastErr = nil
	c.diagnostic = ""

	go run.readLoop()
	go run.stderrLoop()
	go run.waitLoop()

	go func() {
		select {
		case <-ctx.Done():
			_ = c.stopRun(run)
		case <-run.done:
		}
	}()

	return nil
}

// Frames returns the channel for the current or most recent capture. It is
// closed when the encoder exits.
func (c *FFmpegCapture) Frames() <-chan Frame {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.frames
}

// Stop terminates ffmpeg gracefully, escalating to a kill after a bounded
// grace period, and reaps the process. It is idempotent.
func (c *FFmpegCapture) Stop() error {
	c.mu.Lock()
	run := c.run
	c.mu.Unlock()
	if run == nil {
		return nil
	}
	return c.stopRun(run)
}

// LastError returns the last reader/start error observed for the capture.
func (c *FFmpegCapture) LastError() error {
	c.mu.Lock()
	run := c.run
	err := c.lastErr
	if run != nil {
		run.errMu.Lock()
		if run.lastErr != nil {
			err = run.lastErr
		}
		run.errMu.Unlock()
	}
	c.mu.Unlock()
	return err
}

// Diagnostics returns the bounded tail of ffmpeg's stderr output. It never
// contains frame bytes.
func (c *FFmpegCapture) Diagnostics() string {
	c.mu.Lock()
	run := c.run
	diagnostic := c.diagnostic
	c.mu.Unlock()
	if run != nil {
		return run.stderrBuf.String()
	}
	return diagnostic
}

// Available reports whether the ffmpeg executable exists.
func (c *FFmpegCapture) Available() error {
	if _, err := c.lookPath("ffmpeg"); err != nil {
		return fmt.Errorf("%w: %v", ErrFFmpegUnavailable, err)
	}
	return nil
}

func (c *FFmpegCapture) stopRun(run *ffmpegRun) error {
	run.stopOnce.Do(func() {
		run.stopping.Store(true)
		close(run.stopCh)
		terminateProcess(run.cmd.Process)
	})

	grace, cancel := context.WithTimeout(context.Background(), stopGracePeriod)
	defer cancel()

	var killErr error
	select {
	case <-run.done:
	case <-grace.Done():
		if err := run.cmd.Process.Kill(); err != nil && !errors.Is(err, os.ErrProcessDone) {
			killErr = fmt.Errorf("remotescreen: kill ffmpeg: %w", err)
		}
		<-run.done
	}

	c.mu.Lock()
	if c.run == run {
		c.run = nil
		c.running = false
		run.errMu.Lock()
		c.lastErr = run.lastErr
		run.errMu.Unlock()
		c.diagnostic = run.stderrBuf.String()
	}
	c.mu.Unlock()

	run.parser.Reset()
	return killErr
}

func (r *ffmpegRun) readLoop() {
	defer close(r.readDone)
	reader := bufio.NewReaderSize(r.stdout, ffmpegReadChunk)
	chunk := make([]byte, ffmpegReadChunk)
	for {
		n, err := reader.Read(chunk)
		if n > 0 {
			units, parseErr := r.parser.Feed(chunk[:n])
			if parseErr != nil {
				r.setError(parseErr)
				r.stopping.Store(true)
				_ = r.cmd.Process.Kill()
				return
			}
			for _, au := range units {
				if !r.sendFrame(au) {
					return
				}
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				if r.stopping.Load() {
					r.parser.Reset()
					return
				}
				for _, au := range r.parser.Flush() {
					if !r.sendFrame(au) {
						return
					}
				}
				return
			}
			if !errors.Is(err, os.ErrClosed) {
				r.setError(fmt.Errorf("remotescreen: read ffmpeg stdout: %w", err))
			}
			return
		}
	}
}

func (r *ffmpegRun) sendFrame(au AccessUnit) bool {
	frame := Frame{
		Data:     au.AnnexB(),
		PTS:      time.Duration(r.ptsIndex) * time.Second / time.Duration(r.fps),
		Keyframe: au.Keyframe,
	}
	r.ptsIndex++
	select {
	case r.frames <- frame:
		return true
	case <-r.stopCh:
		return false
	case <-r.done:
		return false
	}
}

func (r *ffmpegRun) stderrLoop() {
	defer close(r.stderrDone)
	buf := make([]byte, 4096)
	for {
		n, err := r.stderr.Read(buf)
		if n > 0 {
			r.stderrBuf.Write(buf[:n])
		}
		if err != nil {
			return
		}
	}
}

func (r *ffmpegRun) waitLoop() {
	<-r.readDone
	<-r.stderrDone
	err := r.cmd.Wait()
	if err != nil && !r.stopping.Load() {
		r.setError(fmt.Errorf("remotescreen: ffmpeg exited: %w", err))
	}
	close(r.frames)
	close(r.done)
	if r.parent != nil {
		r.parent.runExited(r)
	}
}

// runExited records diagnostics when ffmpeg exits on its own and marks the
// capture idle so it can be started again.
func (c *FFmpegCapture) runExited(run *ffmpegRun) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.run != run {
		return
	}
	run.errMu.Lock()
	c.lastErr = run.lastErr
	run.errMu.Unlock()
	c.diagnostic = run.stderrBuf.String()
	c.run = nil
	c.running = false
}

func (r *ffmpegRun) setError(err error) {
	r.errMu.Lock()
	r.lastErr = err
	r.errMu.Unlock()
}

// boundedBuffer keeps the most recent bytes of ffmpeg diagnostics.
type boundedBuffer struct {
	mu    sync.Mutex
	data  []byte
	limit int
}

func newBoundedBuffer(limit int) *boundedBuffer {
	return &boundedBuffer{limit: limit}
}

func (b *boundedBuffer) Write(p []byte) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.limit <= 0 {
		return
	}
	if len(p) >= b.limit {
		b.data = append(b.data[:0], p[len(p)-b.limit:]...)
		return
	}
	overflow := len(b.data) + len(p) - b.limit
	if overflow > 0 {
		copy(b.data, b.data[overflow:])
		b.data = b.data[:len(b.data)-overflow]
	}
	b.data = append(b.data, p...)
}

func (b *boundedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return strings.TrimSpace(string(b.data))
}
