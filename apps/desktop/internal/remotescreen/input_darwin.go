//go:build darwin

package remotescreen

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"
)

const helperReadyTimeout = 3 * time.Second

// helperInput drives the small ukk-input helper over JSON lines on stdin.
// The helper owns the macOS Accessibility/CoreGraphics calls; this process
// never loads a private framework or uses cgo.
type helperInput struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stderr *boundedBuffer

	mu    sync.Mutex
	done  chan struct{}
	close sync.Once

	accessibility atomic.Bool

	errMu   sync.Mutex
	lastErr error

	doneOnce sync.Once
}

type helperMessage struct {
	Type          string `json:"type"`
	Accessibility bool   `json:"accessibility"`
	Message       string `json:"message"`
}

type helperCommand struct {
	Type   string `json:"type"`
	Kind   string `json:"kind,omitempty"`
	X      int    `json:"x,omitempty"`
	Y      int    `json:"y,omitempty"`
	Button string `json:"button,omitempty"`
	Code   string `json:"code,omitempty"`
	Down   bool   `json:"down"`
	DX     int    `json:"dx,omitempty"`
	DY     int    `json:"dy,omitempty"`
}

func newPlatformInput() (Input, error) {
	path, err := findInputHelper()
	if err != nil {
		return nil, err
	}

	cmd := exec.Command(path) // argv only; no shell
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("%w: helper stdin: %v", ErrInputUnavailable, err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("%w: helper stdout: %v", ErrInputUnavailable, err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("%w: helper stderr: %v", ErrInputUnavailable, err)
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("%w: start helper: %v", ErrInputUnavailable, err)
	}

	h := &helperInput{
		cmd:    cmd,
		stdin:  stdin,
		stderr: newBoundedBuffer(ffmpegStderrLimit),
		done:   make(chan struct{}),
	}

	ready := make(chan helperMessage, 1)
	scanDone := make(chan struct{})
	go h.scanStdout(stdout, ready, scanDone)
	stderrDone := make(chan struct{})
	go func() {
		defer close(stderrDone)
		buf := make([]byte, 4096)
		for {
			n, err := stderr.Read(buf)
			if n > 0 {
				h.stderr.Write(buf[:n])
			}
			if err != nil {
				return
			}
		}
	}()
	go func() {
		<-scanDone
		<-stderrDone
		err := cmd.Wait()
		if err != nil {
			h.setError(fmt.Errorf("remotescreen: ukk-input exited: %w", err))
		}
		h.closeDone()
	}()

	select {
	case msg := <-ready:
		if msg.Type != "ready" {
			h.killAndWait()
			return nil, fmt.Errorf("%w: helper did not report ready: %s", ErrInputUnavailable, msg.Message)
		}
		h.accessibility.Store(msg.Accessibility)
		return h, nil
	case <-h.done:
		return nil, fmt.Errorf("%w: helper exited before ready (%s)", ErrInputUnavailable, h.stderr.String())
	case <-time.After(helperReadyTimeout):
		h.killAndWait()
		return nil, fmt.Errorf("%w: helper ready timeout", ErrInputUnavailable)
	}
}

func (h *helperInput) scanStdout(stdout io.Reader, ready chan<- helperMessage, done chan<- struct{}) {
	defer close(done)
	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 1024), 4096)
	first := true
	for scanner.Scan() {
		var msg helperMessage
		if err := json.Unmarshal(scanner.Bytes(), &msg); err != nil {
			h.setError(fmt.Errorf("remotescreen: malformed helper output: %w", err))
			continue
		}
		if first {
			first = false
			ready <- msg
		} else if msg.Type == "error" {
			h.setError(fmt.Errorf("remotescreen: ukk-input: %s", msg.Message))
		}
	}
}

func (h *helperInput) killAndWait() {
	_ = h.cmd.Process.Kill()
	<-h.done
}

func (h *helperInput) closeDone() {
	h.doneOnce.Do(func() { close(h.done) })
}

func (h *helperInput) setError(err error) {
	h.errMu.Lock()
	h.lastErr = err
	h.errMu.Unlock()
}

// OSPermission implements OSPermission for GrantAuthorizer wiring.
func (h *helperInput) OSPermission() error {
	if !h.accessibility.Load() {
		return fmt.Errorf("%w: grant Accessibility to ukk", ErrOSPermissionDenied)
	}
	return nil
}

func (h *helperInput) Pointer(kind PointerKind, x, y int, button Button) error {
	if kind != PointerMove && kind != PointerDown && kind != PointerUp {
		return fmt.Errorf("%w: pointer kind %q", ErrInvalidInput, kind)
	}
	return h.send(helperCommand{
		Type:   "pointer",
		Kind:   string(kind),
		X:      x,
		Y:      y,
		Button: string(button),
	})
}

func (h *helperInput) Key(code KeyCode, down bool) error {
	if !validKeyCode(code) {
		return fmt.Errorf("%w: %q", ErrUnsupportedKey, code)
	}
	return h.send(helperCommand{Type: "key", Code: string(code), Down: down})
}

func (h *helperInput) Wheel(dx, dy int) error {
	return h.send(helperCommand{Type: "wheel", DX: dx, DY: dy})
}

func (h *helperInput) send(cmd helperCommand) error {
	if err := h.OSPermission(); err != nil {
		return err
	}
	data, err := json.Marshal(cmd)
	if err != nil {
		return fmt.Errorf("remotescreen: encode helper command: %w", err)
	}
	data = append(data, '\n')

	h.mu.Lock()
	defer h.mu.Unlock()
	select {
	case <-h.done:
		return fmt.Errorf("%w: helper not running", ErrInputUnavailable)
	default:
	}
	if _, err := h.stdin.Write(data); err != nil {
		return fmt.Errorf("%w: helper stdin: %v", ErrInputUnavailable, err)
	}
	return nil
}

// Close terminates the helper and reaps it.
func (h *helperInput) Close() error {
	h.close.Do(func() {
		h.mu.Lock()
		_ = h.stdin.Close()
		h.mu.Unlock()
		select {
		case <-h.done:
		case <-time.After(stopGracePeriod):
			_ = h.cmd.Process.Kill()
			<-h.done
		}
	})
	return nil
}

func findInputHelper() (string, error) {
	if env := os.Getenv("UKK_INPUT_HELPER"); env != "" {
		if usableHelper(env) {
			return env, nil
		}
		return "", fmt.Errorf("%w: UKK_INPUT_HELPER=%s is not executable", ErrInputHelperMissing, env)
	}
	exe, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("%w: locate executable: %v", ErrInputHelperMissing, err)
	}
	dir := filepath.Dir(exe)
	candidates := []string{
		filepath.Join(dir, "resources", "ukk-input"),
		filepath.Join(dir, "..", "resources", "ukk-input"),
		filepath.Join(dir, "..", "Resources", "ukk-input"),
		filepath.Join("resources", "ukk-input"),
	}
	for _, candidate := range candidates {
		if usableHelper(candidate) {
			return candidate, nil
		}
	}
	return "", fmt.Errorf("%w: expected resources/ukk-input", ErrInputHelperMissing)
}

func usableHelper(path string) bool {
	info, err := os.Stat(path)
	if err != nil {
		return false
	}
	return info.Mode().IsRegular() && info.Mode().Perm()&0o111 != 0
}
