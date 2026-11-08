package agents

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// errFrameTooLong is returned when an unterminated line exceeds the drain
// budget. The stream is considered broken and the child is stopped.
var errFrameTooLong = errors.New("line exceeded the drain limit")

// processConfig is everything needed to own one omp child.
type processConfig struct {
	binary   string
	args     []string
	cwd      string
	env      map[string]string
	frameCap int

	onLine    func(line []byte) error
	onStderr  func(line string)
	onFailure func(cause error)
}

// ompProcess owns exactly one child process and its pipes. Every terminal path
// (EOF, read error, write error, startup failure, explicit stop) funnels into
// terminate, which kills and reaps the child exactly once.
type ompProcess struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stdout io.ReadCloser
	stderr io.ReadCloser

	frameCap atomic.Int64
	writeMu  sync.Mutex

	done     chan struct{}
	waitErr  error
	killOnce sync.Once
	failOnce sync.Once

	onLine    func(line []byte) error
	onStderr  func(line string)
	onFailure func(cause error)
}

// startProcess spawns the child, starts its readers, and returns immediately.
// Startup readiness is decided by the caller from onLine/onFailure.
func startProcess(cfg processConfig) (*ompProcess, error) {
	stdinR, stdinW, err := os.Pipe()
	if err != nil {
		return nil, NewError(CodeStartupFailed, "could not create omp stdin pipe: %s", boundedMessage(err.Error(), 256))
	}
	stdoutR, stdoutW, err := os.Pipe()
	if err != nil {
		closeAll(stdinR, stdinW)
		return nil, NewError(CodeStartupFailed, "could not create omp stdout pipe: %s", boundedMessage(err.Error(), 256))
	}
	stderrR, stderrW, err := os.Pipe()
	if err != nil {
		closeAll(stdinR, stdinW, stdoutR, stdoutW)
		return nil, NewError(CodeStartupFailed, "could not create omp stderr pipe: %s", boundedMessage(err.Error(), 256))
	}

	cmd := exec.Command(cfg.binary, cfg.args...)
	if cfg.cwd != "" {
		cmd.Dir = cfg.cwd
	}
	cmd.Env = mergedEnv(cfg.env)
	cmd.Stdin = stdinR
	cmd.Stdout = stdoutW
	cmd.Stderr = stderrW

	if err := cmd.Start(); err != nil {
		closeAll(stdinR, stdinW, stdoutR, stdoutW, stderrR, stderrW)
		return nil, NewError(CodeStartupFailed, "could not start %s: %s", cfg.binary, boundedMessage(err.Error(), 512))
	}
	// The child owns these ends now.
	closeAll(stdinR, stdoutW, stderrW)

	frameCap := cfg.frameCap
	if frameCap < minFrameBytes {
		frameCap = defaultMaxFrameBytes
	}
	process := &ompProcess{
		cmd:       cmd,
		stdin:     stdinW,
		stdout:    stdoutR,
		stderr:    stderrR,
		done:      make(chan struct{}),
		onLine:    cfg.onLine,
		onStderr:  cfg.onStderr,
		onFailure: cfg.onFailure,
	}
	process.frameCap.Store(int64(frameCap))

	go process.wait()
	return process, nil
}

// startReaders begins consuming stdout and stderr. The caller must assign the
// process to its session first so callbacks can use it.
func (p *ompProcess) startReaders() {
	go p.readStdout()
	go p.readStderr()
}

func (p *ompProcess) wait() {
	p.waitErr = p.cmd.Wait()
	close(p.done)
}

func (p *ompProcess) frameCapBytes() int { return int(p.frameCap.Load()) }

func (p *ompProcess) setFrameCap(advertised int) {
	configured := int(p.frameCap.Load())
	if advertised < minFrameBytes {
		advertised = minFrameBytes
	}
	if advertised > absoluteMaxFrameBytes {
		advertised = absoluteMaxFrameBytes
	}
	if configured < minFrameBytes {
		configured = defaultMaxFrameBytes
	}
	if advertised > configured {
		advertised = configured
	}
	p.frameCap.Store(int64(advertised))
}

func (p *ompProcess) exited() bool {
	select {
	case <-p.done:
		return true
	default:
		return false
	}
}

// exitErr returns the Wait result once the child is reaped.
func (p *ompProcess) exitErr() error {
	select {
	case <-p.done:
		return p.waitErr
	default:
		return nil
	}
}

// sendFrame writes one JSON line. It never logs the frame: prompts and tool
// input may carry user content.
func (p *ompProcess) sendFrame(frame any) error {
	data, err := json.Marshal(frame)
	if err != nil {
		return NewError(CodeInvalidArgument, "could not encode the omp command: %s", boundedMessage(err.Error(), 256))
	}
	limit := p.frameCapBytes()
	if len(data)+1 > limit {
		return NewError(CodeInvalidArgument, "command frame exceeds the %d-byte omp transport limit", limit)
	}
	data = append(data, '\n')

	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	if _, err := p.stdin.Write(data); err != nil {
		return NewError(CodeTransport, "writing to omp failed: %s", boundedMessage(err.Error(), 256))
	}
	return nil
}

// terminate closes stdin, gives the child a grace period, then kills and reaps
// it. It is idempotent and safe to call from any goroutine.
func (p *ompProcess) terminate(grace time.Duration) error {
	p.killOnce.Do(func() {
		_ = p.stdin.Close()
		if grace > 0 {
			timer := time.NewTimer(grace)
			select {
			case <-p.done:
				timer.Stop()
				return
			case <-timer.C:
			}
		}
		if p.cmd.Process != nil {
			_ = p.cmd.Process.Kill()
		}
		// Wait is already running in its own goroutine; bound the reap so a
		// wedged kernel call cannot hang shutdown forever.
		timer := time.NewTimer(5 * time.Second)
		defer timer.Stop()
		select {
		case <-p.done:
		case <-timer.C:
		}
	})
	return p.exitErr()
}

// fail records the first transport failure, stops the child, and reports the
// cause. Later failures are ignored so the session settles exactly once.
func (p *ompProcess) fail(cause error) {
	p.failOnce.Do(func() {
		_ = p.terminate(0)
		if p.onFailure != nil {
			p.onFailure(cause)
		}
	})
}

func (p *ompProcess) readStdout() {
	defer p.stdout.Close()
	reader := bufio.NewReaderSize(p.stdout, 64<<10)
	for {
		limit := p.frameCapBytes()
		line, oversized, err := readBoundedLine(reader, limit, limit*frameDrainFactor)
		if oversized {
			p.fail(NewError(CodeProtocol, "omp frame exceeded the %d-byte transport limit", limit))
			return
		}
		if len(line) > 0 {
			if lineErr := p.onLine(line); lineErr != nil {
				p.fail(lineErr)
				return
			}
		}
		if err == nil {
			continue
		}
		if errors.Is(err, io.EOF) {
			p.fail(NewError(CodeTransport, "omp closed stdout"))
			return
		}
		p.fail(NewError(CodeTransport, "reading omp stdout failed: %s", boundedMessage(err.Error(), 256)))
		return
	}
}

func (p *ompProcess) readStderr() {
	defer p.stderr.Close()
	reader := bufio.NewReaderSize(p.stderr, 16<<10)
	for {
		line, oversized, err := readBoundedLine(reader, maxStderrLineBytes, maxStderrDrainBytes)
		if oversized && p.onStderr != nil {
			p.onStderr("[stderr line exceeded the diagnostic limit]")
		} else if len(line) > 0 && p.onStderr != nil {
			p.onStderr(string(line))
		}
		if err == nil {
			continue
		}
		if errors.Is(err, io.EOF) {
			return
		}
		// Only a stream failure (not a long but drained line) stops the child.
		p.fail(NewError(CodeTransport, "reading omp stderr failed: %s", boundedMessage(err.Error(), 256)))
		return
	}
}

// readBoundedLine reads one newline-terminated line, keeping at most max bytes.
// An oversized line is drained through its newline so the stream can continue;
// if the drain exceeds drainCap the line is reported as errFrameTooLong.
func readBoundedLine(reader *bufio.Reader, max, drainCap int) ([]byte, bool, error) {
	if max < 1 {
		max = 1
	}
	line := make([]byte, 0, min(max, 4096))
	oversized := false
	drained := 0
	for {
		chunk, err := reader.ReadSlice('\n')
		if len(chunk) > 0 {
			if !oversized {
				remaining := max - len(line)
				if len(chunk) > remaining {
					line = append(line, chunk[:remaining]...)
					oversized = true
					drained = len(chunk) - remaining
				} else {
					line = append(line, chunk...)
				}
			} else {
				drained += len(chunk)
			}
			if oversized && drained > drainCap {
				return nil, false, errFrameTooLong
			}
		}
		switch {
		case err == nil:
			if oversized {
				return nil, true, nil
			}
			return bytes.TrimRight(line, "\r\n"), false, nil
		case errors.Is(err, bufio.ErrBufferFull):
			continue
		case errors.Is(err, io.EOF):
			if oversized {
				return nil, true, io.EOF
			}
			if len(line) == 0 {
				return nil, false, io.EOF
			}
			return bytes.TrimRight(line, "\r\n"), false, io.EOF
		default:
			return nil, false, err
		}
	}
}

// mergedEnv overlays extra onto the current environment deterministically.
func mergedEnv(extra map[string]string) []string {
	values := make(map[string]string, len(os.Environ())+len(extra))
	keys := make([]string, 0, len(os.Environ())+len(extra))
	add := func(key, value string) {
		if _, seen := values[key]; !seen {
			keys = append(keys, key)
		}
		values[key] = value
	}
	for _, entry := range os.Environ() {
		key, value, ok := strings.Cut(entry, "=")
		if !ok || key == "" {
			continue
		}
		add(key, value)
	}
	for key, value := range extra {
		if key == "" {
			continue
		}
		add(key, value)
	}
	sort.Strings(keys)
	out := make([]string, 0, len(keys))
	for _, key := range keys {
		out = append(out, key+"="+values[key])
	}
	return out
}

func closeAll(files ...io.Closer) {
	for _, file := range files {
		if file != nil {
			_ = file.Close()
		}
	}
}

// describeExit renders a bounded, redacted child exit detail.
func describeExit(err error) string {
	if err == nil {
		return "omp exited"
	}
	var exitErr *exec.ExitError
	if errors.As(err, &exitErr) {
		if code := exitErr.ExitCode(); code >= 0 {
			return fmt.Sprintf("omp exited with code %d", code)
		}
		return boundedMessage(fmt.Sprintf("omp exited: %v", exitErr), 256)
	}
	return boundedMessage(fmt.Sprintf("omp exited: %v", err), 256)
}
