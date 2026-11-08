package hc

import (
	"bufio"
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"syscall"
	"testing"
	"time"
)

// The helper process is this test binary re-executed with
// GO_WANT_HELPER_PROCESS=1. It speaks the real wire protocol: one plain JSON
// hello line, then framed JSON-RPC and stream frames.
func TestMain(m *testing.M) {
	if os.Getenv("GO_WANT_HELPER_PROCESS") == "1" {
		runFakeHC()
		os.Exit(0)
	}
	os.Exit(m.Run())
}

type fakeLogEntry struct {
	Tag     string          `json:"tag"`
	Method  string          `json:"method,omitempty"`
	Params  json.RawMessage `json:"params,omitempty"`
	Channel uint32          `json:"channel,omitempty"`
	Kind    uint8           `json:"kind,omitempty"`
	Payload []byte          `json:"payload,omitempty"`
}

var fakeLogMu sync.Mutex

func fakeLog(path string, entry fakeLogEntry) {
	if path == "" {
		return
	}
	body, err := json.Marshal(entry)
	if err != nil {
		return
	}
	body = append(body, '\n')
	fakeLogMu.Lock()
	defer fakeLogMu.Unlock()
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return
	}
	_, _ = f.Write(body)
	_ = f.Close()
}

func runFakeHC() {
	mode := os.Getenv("HC_FAKE_MODE")
	logPath := os.Getenv("HC_FAKE_LOG")
	if pidPath := os.Getenv("HC_FAKE_PID"); pidPath != "" {
		_ = os.WriteFile(pidPath, []byte(fmt.Sprintf("%d\n", os.Getpid())), 0o600)
	}
	if mode == "hang" {
		for {
			time.Sleep(time.Hour)
		}
	}

	br := bufio.NewReader(os.Stdin)
	bw := bufio.NewWriter(os.Stdout)
	helloLine, err := br.ReadString('\n')
	if err != nil {
		return
	}
	var hello map[string]any
	_ = json.Unmarshal([]byte(helloLine), &hello)
	fakeLog(logPath, fakeLogEntry{Tag: "hello", Method: "hello", Params: json.RawMessage(helloLine)})

	framed := true
	if mode == "plain-reply" {
		framed = false
	}
	reply := map[string]any{
		"jsonrpc": "2.0",
		"id":      1,
		"result": map[string]any{
			"generation": 1,
			"version":    "0.1.18",
			"core":       map[string]any{"id": "fake-core", "name": "fake-host"},
			"framed":     framed,
			"methods":    []string{"hello", "server.status", "terminal.list"},
			"features":   []string{"framed", "attach", "events", "status", "resize_notifications", "automatic_input"},
		},
	}
	body, _ := json.Marshal(reply)
	_, _ = bw.Write(append(body, '\n'))
	_ = bw.Flush()
	if mode == "plain-reply" {
		for {
			time.Sleep(time.Hour)
		}
	}
	if mode == "hang-after-hello" {
		for {
			time.Sleep(time.Hour)
		}
	}
	if mode == "oversized" {
		var header [FrameHeaderSize]byte
		binary.LittleEndian.PutUint32(header[0:4], 1)
		header[4] = KindData
		binary.LittleEndian.PutUint32(header[5:9], uint32(MaxPayload+1))
		_, _ = bw.Write(header[:])
		_ = bw.Flush()
		for {
			time.Sleep(time.Hour)
		}
	}

	streamOpen := true
	for {
		frame, err := ReadFrame(br)
		if err != nil {
			return
		}
		if frame.Channel == 0 {
			handleFakeRequest(bw, logPath, mode, framed, frame.Payload, &streamOpen)
			continue
		}
		fakeLog(logPath, fakeLogEntry{Tag: "frame", Channel: frame.Channel, Kind: frame.Kind, Payload: frame.Payload})
		switch frame.Kind {
		case KindClose:
			writeFakeFrame(bw, frame.Channel, KindClose, nil)
			streamOpen = false
		case KindCredit:
			if mode == "credit" && streamOpen && bytes.Equal(frame.Payload, []byte{4, 0, 0, 0}) {
				writeFakeFrame(bw, frame.Channel, KindData, []byte("abcdefghij"))
			}
		}
	}
}

func handleFakeRequest(bw *bufio.Writer, logPath, mode string, framed bool, payload []byte, streamOpen *bool) {
	var req struct {
		ID     uint64          `json:"id"`
		Method string          `json:"method"`
		Params json.RawMessage `json:"params"`
	}
	if err := json.Unmarshal(payload, &req); err != nil {
		return
	}
	fakeLog(logPath, fakeLogEntry{Tag: "request", Method: req.Method, Params: req.Params})

	result := map[string]any{}
	switch req.Method {
	case "terminal.list":
		result = map[string]any{"terminals": []any{fakeTerminal("t1")}, "hidden_finished": 0, "machines": []any{}}
	case "terminal.get":
		var getParams struct {
			Terminal string `json:"terminal"`
		}
		_ = json.Unmarshal(req.Params, &getParams)
		result = fakeTerminal(getParams.Terminal)
	case "terminal.run":
		term := fakeTerminal("t9")
		for k, v := range map[string]any{"labels": map[string]string{}, "argv": []string{"echo", "hi"}, "cwd": "/tmp", "state": "live", "created_ms": 1, "screen": "exact", "cols": 80, "rows": 24} {
			term[k] = v
		}
		result = term
	case "terminal.send":
		result = map[string]any{"turn": 2}
	case "terminal.keys":
		result = map[string]any{"turn": 3}
	case "terminal.interrupt":
		result = map[string]any{"outcome": "stopped", "key": "ctrl+c"}
	case "terminal.kill", "terminal.keep":
		result = fakeTerminal("t1")
	case "terminal.rm":
		result = map[string]any{"removed": "t1"}
	case "terminal.read":
		result = map[string]any{"text": "hello", "complete": true, "how": "screen"}
	case "server.status":
		result = map[string]any{
			"core": map[string]any{"id": "fake-core", "name": "fake-host"}, "version": "0.1.18",
			"pid": 1, "uptime_ms": 2, "holders": []any{}, "live": 1, "finished": 0,
		}
	case "terminal.attach":
		channel := uint32(3)
		result = map[string]any{
			"channel": channel, "terminal": fakeTerminal("t1"),
			"resize_notifications": true, "automatic_input": true,
		}
		writeFakeResult(bw, req.ID, result)
		switch mode {
		case "credit":
			writeFakeFrame(bw, channel, KindData, []byte("0123456789"))
		case "close":
			closeReason := map[string]any{"reason": "exited", "exit_code": 7, "signal": nil, "finished_ms": 123}
			body, _ := json.Marshal(closeReason)
			writeFakeFrame(bw, channel, KindClose, body)
		case "core-resize":
			writeFakeFrame(bw, channel, KindResize, []byte{120, 0, 40, 0})
			writeFakeFrame(bw, channel, KindFacts, []byte(`{"fg":"#ffffff"}`))
		case "unknown":
			writeFakeFrame(bw, channel, 200, []byte("ignored kind"))
			writeFakeFrame(bw, channel, KindData, []byte("ok"))
		}
		return
	case "events.subscribe":
		result = map[string]any{"channel": 4, "seq": 10, "terminals": []any{fakeTerminal("t10")}}
		writeFakeResult(bw, req.ID, result)
		if mode == "events" {
			lines := []string{
				`{"jsonrpc":"2.0","method":"event","params":{"seq":12,"type":"title","terminal":"t10","data":{"title":"a"}}}`,
				`{"jsonrpc":"2.0","method":"missed","params":{"seq":20,"terminals":[` + fakeTerminalJSON("t20") + `]}}`,
				`{"jsonrpc":"2.0","method":"event","params":{"seq":19,"type":"title","terminal":"t10","data":{"title":"stale"}}}`,
				`{"jsonrpc":"2.0","method":"event","params":{"seq":21,"type":"title","terminal":"t11","data":{"title":"b"}}}`,
				`{"jsonrpc":"2.0","method":"heartbeat","params":{"seq":21}}`,
			}
			var buf bytes.Buffer
			for _, line := range lines {
				buf.WriteString(line)
				buf.WriteByte('\n')
			}
			writeFakeFrame(bw, 4, KindData, buf.Bytes())
		}
		return
	default:
		writeFakeError(bw, req.ID, -32601, "method not found", "unknown")
		return
	}
	writeFakeResult(bw, req.ID, result)
}

func fakeTerminal(address string) map[string]any {
	var term map[string]any
	_ = json.Unmarshal([]byte(fakeTerminalJSON(address)), &term)
	return term
}

func fakeTerminalJSON(address string) string {
	return `{"address":"` + address + `","labels":{},"argv":["sh"],"cwd":"/tmp","state":"live","created_ms":1,"screen":"exact","cols":80,"rows":24}`
}

func writeFakeResult(bw *bufio.Writer, id uint64, result any) {
	body, _ := json.Marshal(map[string]any{"jsonrpc": "2.0", "id": id, "result": result})
	writeFakeFrame(bw, 0, KindData, body)
}

func writeFakeError(bw *bufio.Writer, id uint64, code int, message, kind string) {
	body, _ := json.Marshal(map[string]any{
		"jsonrpc": "2.0", "id": id,
		"error": map[string]any{"code": code, "message": message, "data": map[string]string{"kind": kind}},
	})
	writeFakeFrame(bw, 0, KindData, body)
}

func writeFakeFrame(bw *bufio.Writer, channel uint32, kind uint8, payload []byte) {
	encoded, err := EncodeFrame(channel, kind, payload)
	if err != nil {
		return
	}
	_, _ = bw.Write(encoded)
	_ = bw.Flush()
}

func readFakeLog(path string) []fakeLogEntry {
	body, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var entries []fakeLogEntry
	for _, line := range bytes.Split(body, []byte{'\n'}) {
		if len(bytes.TrimSpace(line)) == 0 {
			continue
		}
		var entry fakeLogEntry
		if err := json.Unmarshal(line, &entry); err == nil {
			entries = append(entries, entry)
		}
	}
	return entries
}

func waitForLog(t *testing.T, path string, check func([]fakeLogEntry) bool) []fakeLogEntry {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for {
		entries := readFakeLog(path)
		if check(entries) {
			return entries
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for fake server log; entries: %+v", entries)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func findRequest(entries []fakeLogEntry, method string) (fakeLogEntry, bool) {
	for _, entry := range entries {
		if entry.Tag == "request" && entry.Method == method {
			return entry, true
		}
	}
	return fakeLogEntry{}, false
}

func startFake(t *testing.T, mode string) (*Client, string) {
	t.Helper()
	client, _, logPath := startFakeContext(t, mode, 2*time.Second)
	return client, logPath
}

func startFakeContext(t *testing.T, mode string, timeout time.Duration) (*Client, *fakeServer, string) {
	t.Helper()
	dir := t.TempDir()
	logPath := filepath.Join(dir, "log.jsonl")
	pidPath := filepath.Join(dir, "pid")
	server := &fakeServer{logPath: logPath, pidPath: pidPath, mode: mode}
	exe, err := os.Executable()
	if err != nil {
		t.Fatalf("os.Executable: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(func() {
		cancel()
		_ = os.RemoveAll(dir)
	})
	client, err := Connect(ctx, Options{
		Binary:  exe,
		Timeout: timeout,
		Env: []string{
			"GO_WANT_HELPER_PROCESS=1",
			"HC_FAKE_MODE=" + mode,
			"HC_FAKE_LOG=" + logPath,
			"HC_FAKE_PID=" + pidPath,
		},
	})
	if err != nil {
		t.Fatalf("Connect(%s): %v", mode, err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client, server, logPath
}

type fakeServer struct {
	logPath string
	pidPath string
	mode    string
}

func (s *fakeServer) pid(t *testing.T) int {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for {
		body, err := os.ReadFile(s.pidPath)
		if err == nil {
			var pid int
			if _, scanErr := fmt.Sscanf(string(body), "%d", &pid); scanErr == nil && pid > 0 {
				return pid
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("fake server pid file never appeared")
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func executablePath() (string, error) {
	return os.Executable()
}

func readPID(t *testing.T, path string) int {
	t.Helper()
	body, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read pid file: %v", err)
	}
	var pid int
	if _, err := fmt.Sscanf(string(body), "%d", &pid); err != nil || pid <= 0 {
		t.Fatalf("bad pid file %q", string(body))
	}
	return pid
}

func waitPIDGone(t *testing.T, pid int, limit time.Duration) {
	t.Helper()
	deadline := time.Now().Add(limit)
	for {
		err := syscall.Kill(pid, 0)
		if err == syscall.ESRCH {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("helper pid %d still alive after %s (err=%v)", pid, limit, err)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
