package hc

import (
	"context"
	"encoding/json"
	"errors"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestFramedHelloAndRequestShapes(t *testing.T) {
	client, logPath := startFake(t, "framed")
	hello := client.Hello()
	if hello.Generation != 1 || !hello.Framed || hello.Core.ID != "fake-core" || hello.Version != "0.1.18" {
		t.Fatalf("unexpected hello: %+v", hello)
	}
	if !hello.HasFeature("automatic_input") || !hello.HasFeature("resize_notifications") {
		t.Fatalf("features = %v", hello.Features)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	if _, err := client.TerminalList(ctx, TerminalListOptions{All: true, Labels: map[string]string{"role": "review"}}); err != nil {
		t.Fatalf("TerminalList: %v", err)
	}
	if _, err := client.TerminalGet(ctx, "t1", "caller-1"); err != nil {
		t.Fatalf("TerminalGet: %v", err)
	}
	if _, err := client.TerminalRun(ctx, RunOptions{
		Argv:    []string{"echo", "hi"},
		Cwd:     "/tmp",
		Env:     map[string]string{"A": "B"},
		Keep:    true,
		Labels:  map[string]string{"x": "y"},
		Name:    "job",
		Size:    &InitialSize{Cols: 80, Rows: 24},
		Timeout: new(5.0),
		Wait:    true,
	}); err != nil {
		t.Fatalf("TerminalRun: %v", err)
	}
	if _, err := client.ServerStatus(ctx); err != nil {
		t.Fatalf("ServerStatus: %v", err)
	}

	entries := waitForLog(t, logPath, func(entries []fakeLogEntry) bool {
		_, list := findRequest(entries, "terminal.list")
		_, get := findRequest(entries, "terminal.get")
		_, run := findRequest(entries, "terminal.run")
		_, status := findRequest(entries, "server.status")
		return list && get && run && status
	})

	var helloEntry fakeLogEntry
	var ok bool
	for _, entry := range entries {
		if entry.Tag == "hello" {
			helloEntry, ok = entry, true
			break
		}
	}
	if !ok {
		t.Fatal("hello request was not logged")
	}
	var helloEnvelope struct {
		Params map[string]any `json:"params"`
	}
	if err := json.Unmarshal(helloEntry.Params, &helloEnvelope); err != nil {
		t.Fatalf("hello params: %v", err)
	}
	helloParams := helloEnvelope.Params
	if helloParams["framed"] != true || helloParams["generation"] != float64(1) {
		t.Fatalf("hello params = %v", helloParams)
	}

	list, _ := findRequest(entries, "terminal.list")
	var listParams map[string]any
	_ = json.Unmarshal(list.Params, &listParams)
	if listParams["all"] != true || listParams["labels"].(map[string]any)["role"] != "review" {
		t.Fatalf("terminal.list params = %v", listParams)
	}

	get, _ := findRequest(entries, "terminal.get")
	var getParams map[string]any
	_ = json.Unmarshal(get.Params, &getParams)
	if getParams["terminal"] != "t1" || getParams["caller"] != "caller-1" {
		t.Fatalf("terminal.get params = %v", getParams)
	}

	run, _ := findRequest(entries, "terminal.run")
	var runParams map[string]any
	_ = json.Unmarshal(run.Params, &runParams)
	argv, _ := runParams["argv"].([]any)
	if len(argv) != 2 || argv[0] != "echo" || argv[1] != "hi" {
		t.Fatalf("terminal.run argv = %v", runParams["argv"])
	}
	if runParams["cwd"] != "/tmp" || runParams["name"] != "job" || runParams["keep"] != true || runParams["wait"] != true {
		t.Fatalf("terminal.run params = %v", runParams)
	}
	if runParams["env"].(map[string]any)["A"] != "B" || runParams["labels"].(map[string]any)["x"] != "y" {
		t.Fatalf("terminal.run env/labels = %v", runParams)
	}
	size := runParams["size"].(map[string]any)
	if size["cols"] != float64(80) || size["rows"] != float64(24) {
		t.Fatalf("terminal.run size = %v", size)
	}
	if runParams["timeout"] != float64(5) {
		t.Fatalf("terminal.run timeout = %v", runParams["timeout"])
	}
}

func TestPlainHelloRejectedWhenFramingNotConfirmed(t *testing.T) {
	dir := t.TempDir()
	pidPath := dir + "/pid"
	exe, err := executablePath()
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	start := time.Now()
	_, err = Connect(ctx, Options{
		Binary:  exe,
		Timeout: time.Second,
		Env: []string{
			"GO_WANT_HELPER_PROCESS=1",
			"HC_FAKE_MODE=plain-reply",
			"HC_FAKE_LOG=" + dir + "/log.jsonl",
			"HC_FAKE_PID=" + pidPath,
		},
	})
	if err == nil {
		t.Fatal("Connect accepted a plain-mode hello")
	}
	if !IsCode(err, CodeHandshake) {
		t.Fatalf("Connect error = %v, want handshake failure", err)
	}
	if time.Since(start) > 2*time.Second {
		t.Fatalf("handshake rejection took %s", time.Since(start))
	}
	pid := readPID(t, pidPath)
	waitPIDGone(t, pid, 2*time.Second)
}

func TestChildKilledAndReapedOnHandshakeTimeout(t *testing.T) {
	dir := t.TempDir()
	pidPath := dir + "/pid"
	exe, err := executablePath()
	if err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	_, err = Connect(context.Background(), Options{
		Binary:  exe,
		Timeout: 200 * time.Millisecond,
		Env: []string{
			"GO_WANT_HELPER_PROCESS=1",
			"HC_FAKE_MODE=hang",
			"HC_FAKE_LOG=" + dir + "/log.jsonl",
			"HC_FAKE_PID=" + pidPath,
		},
	})
	if !IsCode(err, CodeTimeout) {
		t.Fatalf("Connect error = %v, want timeout", err)
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("timeout path took %s", elapsed)
	}
	pid := readPID(t, pidPath)
	waitPIDGone(t, pid, 2*time.Second)
}

func TestChildKilledAndReapedOnContextCancel(t *testing.T) {
	dir := t.TempDir()
	pidPath := dir + "/pid"
	exe, err := executablePath()
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	client, err := Connect(ctx, Options{
		Binary:  exe,
		Timeout: 5 * time.Second,
		Env: []string{
			"GO_WANT_HELPER_PROCESS=1",
			"HC_FAKE_MODE=hang-after-hello",
			"HC_FAKE_LOG=" + dir + "/log.jsonl",
			"HC_FAKE_PID=" + pidPath,
		},
	})
	if err != nil {
		cancel()
		t.Fatalf("Connect: %v", err)
	}
	pid := readPID(t, pidPath)
	callErr := make(chan error, 1)
	go func() {
		_, err := client.TerminalList(context.Background(), TerminalListOptions{})
		callErr <- err
	}()
	time.Sleep(20 * time.Millisecond)
	cancel()
	select {
	case err := <-callErr:
		if err == nil {
			t.Fatal("pending call succeeded after context cancellation")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("pending call did not unblock after context cancellation")
	}
	select {
	case <-client.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("client did not report connection loss")
	}
	_ = client.Close()
	waitPIDGone(t, pid, 2*time.Second)
}

func TestNoGoroutineLeakAfterClose(t *testing.T) {
	exe, err := executablePath()
	if err != nil {
		t.Fatal(err)
	}
	baseline := runtime.NumGoroutine()
	for i := 0; i < 3; i++ {
		dir := t.TempDir()
		client, err := Connect(context.Background(), Options{
			Binary:  exe,
			Timeout: 2 * time.Second,
			Env: []string{
				"GO_WANT_HELPER_PROCESS=1",
				"HC_FAKE_MODE=framed",
				"HC_FAKE_LOG=" + dir + "/log.jsonl",
				"HC_FAKE_PID=" + dir + "/pid",
			},
		})
		if err != nil {
			t.Fatalf("Connect: %v", err)
		}
		if _, err := client.TerminalList(context.Background(), TerminalListOptions{}); err != nil {
			t.Fatalf("TerminalList: %v", err)
		}
		if err := client.Close(); err != nil {
			t.Fatalf("Close: %v", err)
		}
	}
	deadline := time.Now().Add(2 * time.Second)
	for {
		runtime.GC()
		after := runtime.NumGoroutine()
		if after <= baseline+3 {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("goroutine leak: baseline=%d after=%d", baseline, after)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestConnectionLossIsTyped(t *testing.T) {
	client, _ := startFake(t, "framed")
	if err := client.Close(); err != nil {
		t.Fatal(err)
	}
	_, err := client.TerminalList(context.Background(), TerminalListOptions{})
	if err == nil {
		t.Fatal("call after Close succeeded")
	}
	if !IsCode(err, CodeClosed) && !IsCode(err, CodeConnection) {
		t.Fatalf("call after Close error = %v", err)
	}
}

func TestOversizedRequestRejectedBeforeSend(t *testing.T) {
	client, logPath := startFake(t, "framed")
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, err := client.Send(ctx, SendOptions{Terminal: "t1", Text: strings.Repeat("x", maxRequestJSON+1)})
	if !IsCode(err, CodeRequestTooLarge) {
		t.Fatalf("Send error = %v, want request too large", err)
	}
	entries := readFakeLog(logPath)
	if _, ok := findRequest(entries, "terminal.send"); ok {
		t.Fatal("oversized request was sent to the core")
	}
}

func TestOversizedInboundFrameClosesConnection(t *testing.T) {
	client, server, _ := startFakeContext(t, "oversized", 2*time.Second)
	select {
	case <-client.Done():
	case <-time.After(3 * time.Second):
		t.Fatal("client did not detect the oversized inbound frame")
	}
	if err := client.Err(); !IsCode(err, CodePayloadTooLarge) {
		t.Fatalf("terminal error = %v, want payload too large", err)
	}
	pid := server.pid(t)
	_ = client.Close()
	waitPIDGone(t, pid, 2*time.Second)
}

func TestConcurrentRequestsCorrelateByID(t *testing.T) {
	client, _ := startFake(t, "framed")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	const calls = 16
	errs := make(chan error, calls)
	for i := range calls {
		go func() {
			want := "t" + string(rune('a'+i))
			term, err := client.TerminalGet(ctx, want, "")
			if err != nil {
				errs <- err
				return
			}
			if term.Address != want {
				errs <- errors.New("response for the wrong request id: got " + term.Address)
				return
			}
			errs <- nil
		}()
	}
	for range calls {
		if err := <-errs; err != nil {
			t.Fatal(err)
		}
	}
}

func TestPerCallDeadline(t *testing.T) {
	client, _ := startFake(t, "hang-after-hello")
	start := time.Now()
	_, err := client.TerminalList(context.Background(), TerminalListOptions{})
	if !IsCode(err, CodeTimeout) {
		t.Fatalf("TerminalList error = %v, want timeout", err)
	}
	if elapsed := time.Since(start); elapsed > 3*time.Second {
		t.Fatalf("per-call deadline took %s", elapsed)
	}
	_ = client.Close()
}
