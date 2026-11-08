package relay

import (
	"context"
	"encoding/base64"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

const testToken = "test-operator-token-0123456789abcdef"

type fixture struct {
	srv    *Server
	http   *httptest.Server
	events chan Event
}

func newFixture(t *testing.T, mutate func(*Config)) *fixture {
	t.Helper()
	events := make(chan Event, 512)
	cfg := Config{
		OperatorToken: testToken,
		OnEvent: func(event Event) {
			select {
			case events <- event:
			default:
			}
		},
	}
	if mutate != nil {
		mutate(&cfg)
	}
	srv, err := New(cfg)
	if err != nil {
		t.Fatalf("new relay: %v", err)
	}
	ts := httptest.NewServer(srv.Handler())
	f := &fixture{srv: srv, http: ts, events: events}
	t.Cleanup(func() {
		_ = srv.Shutdown(context.Background())
		ts.Close()
	})
	return f
}

func (f *fixture) wsURL(path string) string {
	return "ws" + strings.TrimPrefix(f.http.URL, "http") + path
}

func (f *fixture) waitEvent(t *testing.T, kind EventKind, hostID, deviceID string) Event {
	t.Helper()
	deadline := time.After(3 * time.Second)
	for {
		select {
		case event := <-f.events:
			if event.Kind == kind && (hostID == "" || event.HostID == hostID) && (deviceID == "" || event.DeviceID == deviceID) {
				return event
			}
		case <-deadline:
			t.Fatalf("timed out waiting for event %s host=%s device=%s", kind, hostID, deviceID)
		}
	}
}

func hostHeader(token string) http.Header {
	h := http.Header{}
	if token != "" {
		h.Set("Authorization", "Bearer "+token)
	}
	return h
}

func dialWS(t *testing.T, url string, header http.Header) (*websocket.Conn, *http.Response, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	conn, resp, err := websocket.Dial(ctx, url, &websocket.DialOptions{HTTPHeader: header})
	if err == nil {
		t.Cleanup(func() { _ = conn.CloseNow() })
	}
	return conn, resp, err
}

func writeFrame(conn *websocket.Conn, f frame) error {
	raw, err := encodeFrame(f)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	return conn.Write(ctx, websocket.MessageText, raw)
}

func readFrame(conn *websocket.Conn) (frame, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	typ, raw, err := conn.Read(ctx)
	if err != nil {
		return frame{}, err
	}
	if typ != websocket.MessageText {
		return frame{}, fmt.Errorf("unexpected message type %d", typ)
	}
	f, ferr := decodeFrame(raw)
	if ferr != nil {
		return frame{}, ferr
	}
	return f, nil
}

func mustWrite(t *testing.T, conn *websocket.Conn, f frame) {
	t.Helper()
	if err := writeFrame(conn, f); err != nil {
		t.Fatalf("write frame %s: %v", f.T, err)
	}
}

func expectType(t *testing.T, conn *websocket.Conn, want string) frame {
	t.Helper()
	for i := 0; i < 64; i++ {
		f, err := readFrame(conn)
		if err != nil {
			t.Fatalf("read while expecting %s: %v", want, err)
		}
		if f.T == want {
			return f
		}
		if f.T == "error" && want != "error" {
			t.Fatalf("got error frame %s while expecting %s", f.Code, want)
		}
	}
	t.Fatalf("did not receive %s", want)
	return frame{}
}

func connectHost(t *testing.T, f *fixture, hostID string) *websocket.Conn {
	t.Helper()
	conn, resp, err := dialWS(t, f.wsURL("/host"), hostHeader(testToken))
	if err != nil {
		t.Fatalf("dial host: %v (response %v)", err, resp)
	}
	mustWrite(t, conn, frame{T: "hello", HostID: hostID})
	f.waitEvent(t, EventHostConnected, hostID, "")
	return conn
}

func connectDevice(t *testing.T, f *fixture, hostID, deviceID string) *websocket.Conn {
	t.Helper()
	conn, resp, err := dialWS(t, f.wsURL("/device"), nil)
	if err != nil {
		t.Fatalf("dial device: %v (response %v)", err, resp)
	}
	mustWrite(t, conn, frame{T: "hello", HostID: hostID, DeviceID: deviceID})
	f.waitEvent(t, EventDeviceConnected, hostID, deviceID)
	return conn
}

func TestRelayConfigValidation(t *testing.T) {
	cases := map[string]Config{
		"missing":  {},
		"short":    {OperatorToken: "too-short"},
		"repeated": {OperatorToken: strings.Repeat("a", 32)},
	}
	for name, cfg := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := New(cfg); !HasCode(err, CodeConfigInvalid) {
				t.Fatalf("New error = %v, want config_invalid", err)
			}
		})
	}
	if _, err := New(Config{OperatorToken: testToken}); err != nil {
		t.Fatalf("valid token rejected: %v", err)
	}
}

func TestRelayHealthAndOperatorAuth(t *testing.T) {
	f := newFixture(t, nil)

	resp, err := http.Get(f.http.URL + "/healthz")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK || strings.TrimSpace(string(body)) != "ok" {
		t.Fatalf("healthz = %d %q", resp.StatusCode, body)
	}

	if _, resp, err := dialWS(t, f.wsURL("/host"), nil); err == nil {
		t.Fatal("host without token connected")
	} else if resp == nil || resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %v, want 401", resp)
	}
	if _, resp, err := dialWS(t, f.wsURL("/host"), hostHeader("wrong-token-wrong-token-wrong-token")); err == nil {
		t.Fatal("host with wrong token connected")
	} else if resp == nil || resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %v, want 401", resp)
	}

	host := connectHost(t, f, "host-1")
	mustWrite(t, host, frame{T: "hello", HostID: "host-1"})
	errFrame := expectType(t, host, "error")
	if errFrame.Code != string(CodeDuplicateHello) {
		t.Fatalf("duplicate hello code = %q", errFrame.Code)
	}
	if _, err := readFrame(host); err == nil {
		t.Fatal("connection stayed open after duplicate hello")
	}
}

func TestRelayRoutesBothDirections(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")
	device := connectDevice(t, f, "host-1", "device-1")

	presence := expectType(t, host, "presence")
	if presence.DeviceID != "device-1" || presence.Online == nil || !*presence.Online {
		t.Fatalf("presence = %+v", presence)
	}

	early := base64.StdEncoding.EncodeToString([]byte("too-early"))
	mustWrite(t, device, frame{T: "data", Conn: "conn-1", Payload: early})
	errFrame := expectType(t, device, "error")
	if errFrame.Code != string(CodeUnknownConn) {
		t.Fatalf("data before open code = %q", errFrame.Code)
	}

	mustWrite(t, device, frame{T: "open", Conn: "conn-1"})
	opened := expectType(t, host, "open")
	if opened.Conn != "conn-1" {
		t.Fatalf("open conn = %q", opened.Conn)
	}

	toHost := base64.StdEncoding.EncodeToString([]byte("hello-host"))
	mustWrite(t, device, frame{T: "data", Conn: "conn-1", Payload: toHost})
	data := expectType(t, host, "data")
	if data.Conn != "conn-1" || data.Payload != toHost {
		t.Fatalf("device->host data = %+v", data)
	}
	decoded, err := base64.StdEncoding.DecodeString(data.Payload)
	if err != nil || string(decoded) != "hello-host" {
		t.Fatalf("payload decode = %q, %v", decoded, err)
	}

	raw := make([]byte, 1024)
	for i := range raw {
		raw[i] = byte(i)
	}
	toDevice := base64.StdEncoding.EncodeToString(raw)
	mustWrite(t, host, frame{T: "data", Conn: "conn-1", Payload: toDevice})
	data = expectType(t, device, "data")
	if data.Payload != toDevice {
		t.Fatal("host->device payload changed")
	}
	decoded, err = base64.StdEncoding.DecodeString(data.Payload)
	if err != nil || len(decoded) != len(raw) || decoded[100] != raw[100] {
		t.Fatalf("host->device decode mismatch: %v", err)
	}

	mustWrite(t, device, frame{T: "close", Conn: "conn-1"})
	closed := expectType(t, host, "close")
	if closed.Conn != "conn-1" {
		t.Fatalf("close conn = %q", closed.Conn)
	}

	mustWrite(t, device, frame{T: "data", Conn: "conn-1", Payload: toHost})
	errFrame = expectType(t, device, "error")
	if errFrame.Code != string(CodeUnknownConn) {
		t.Fatalf("data after close code = %q", errFrame.Code)
	}
	mustWrite(t, device, frame{T: "open", Conn: "conn-2"})
	if opened = expectType(t, host, "open"); opened.Conn != "conn-2" {
		t.Fatalf("second open = %+v", opened)
	}

	mustWrite(t, host, frame{T: "data", Conn: "missing", Payload: toHost})
	errFrame = expectType(t, host, "error")
	if errFrame.Code != string(CodeUnknownConn) {
		t.Fatalf("host unknown conn code = %q", errFrame.Code)
	}
	mustWrite(t, host, frame{T: "data", Conn: "conn-2", Payload: toHost})
	if data = expectType(t, device, "data"); data.Payload != toHost {
		t.Fatal("host remained unusable after unknown conn")
	}
}

func TestRelayConnIDsAreUniquePerHost(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")
	first := connectDevice(t, f, "host-1", "device-1")
	second := connectDevice(t, f, "host-1", "device-2")
	_ = expectType(t, host, "presence")
	_ = expectType(t, host, "presence")

	mustWrite(t, first, frame{T: "open", Conn: "shared"})
	if opened := expectType(t, host, "open"); opened.Conn != "shared" {
		t.Fatalf("first open = %+v", opened)
	}
	mustWrite(t, second, frame{T: "open", Conn: "shared"})
	errFrame := expectType(t, second, "error")
	if errFrame.Code != string(CodeDuplicateConn) {
		t.Fatalf("collision code = %q", errFrame.Code)
	}
}

func TestRelayRevokeClosesAndRefuses(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")
	device := connectDevice(t, f, "host-1", "device-1")
	_ = expectType(t, host, "presence")
	mustWrite(t, device, frame{T: "open", Conn: "conn-1"})
	_ = expectType(t, host, "open")

	mustWrite(t, host, frame{T: "revoke", Device: "device-1"})
	f.waitEvent(t, EventDeviceRevoked, "host-1", "device-1")
	presence := expectType(t, host, "presence")
	if presence.Online == nil || *presence.Online {
		t.Fatalf("presence after revoke = %+v", presence)
	}
	errFrame := expectType(t, device, "error")
	if errFrame.Code != string(CodeRevoked) {
		t.Fatalf("revoked code = %q", errFrame.Code)
	}
	if _, err := readFrame(device); err == nil {
		t.Fatal("device connection stayed open after revoke")
	}

	retry, _, err := dialWS(t, f.wsURL("/device"), nil)
	if err != nil {
		t.Fatalf("dial revoked device: %v", err)
	}
	mustWrite(t, retry, frame{T: "hello", HostID: "host-1", DeviceID: "device-1"})
	errFrame = expectType(t, retry, "error")
	if errFrame.Code != string(CodeDeviceRevoked) {
		t.Fatalf("reconnect code = %q", errFrame.Code)
	}

	mustWrite(t, host, frame{T: "repair", Device: "device-1"})
	f.waitEvent(t, EventDeviceRepaired, "host-1", "device-1")
	repaired := connectDevice(t, f, "host-1", "device-1")
	presence = expectType(t, host, "presence")
	if presence.Online == nil || !*presence.Online {
		t.Fatalf("presence after repair = %+v", presence)
	}
	mustWrite(t, repaired, frame{T: "open", Conn: "conn-2"})
	if opened := expectType(t, host, "open"); opened.Conn != "conn-2" {
		t.Fatalf("open after repair = %+v", opened)
	}
}

func TestRelayPayloadLimit(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")
	device := connectDevice(t, f, "host-1", "device-1")
	_ = expectType(t, host, "presence")
	mustWrite(t, device, frame{T: "open", Conn: "conn-1"})
	_ = expectType(t, host, "open")

	tooLarge := strings.Repeat("A", maxEncodedPayload+1)
	mustWrite(t, device, frame{T: "data", Conn: "conn-1", Payload: tooLarge})
	errFrame := expectType(t, device, "error")
	if errFrame.Code != string(CodePayloadTooLarge) {
		t.Fatalf("payload code = %q", errFrame.Code)
	}
	if _, err := readFrame(device); err == nil {
		t.Fatal("connection stayed open after oversized payload")
	}
}

func TestRelayConnLimit(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")
	device := connectDevice(t, f, "host-1", "device-1")
	_ = expectType(t, host, "presence")

	for i := 0; i < MaxConnsPerDevice; i++ {
		connID := fmt.Sprintf("conn-%d", i)
		mustWrite(t, device, frame{T: "open", Conn: connID})
		if opened := expectType(t, host, "open"); opened.Conn != connID {
			t.Fatalf("open %d = %+v", i, opened)
		}
	}
	mustWrite(t, device, frame{T: "open", Conn: "conn-overflow"})
	errFrame := expectType(t, device, "error")
	if errFrame.Code != string(CodeConnLimit) {
		t.Fatalf("conn limit code = %q", errFrame.Code)
	}
	// Non-fatal: the device can still close an existing conn.
	mustWrite(t, device, frame{T: "close", Conn: "conn-0"})
	if closed := expectType(t, host, "close"); closed.Conn != "conn-0" {
		t.Fatalf("close = %+v", closed)
	}
}

func TestRelayDeviceAndHostLimits(t *testing.T) {
	t.Run("device-connections", func(t *testing.T) {
		f := newFixture(t, nil)
		host := connectHost(t, f, "host-1")
		var conns []*websocket.Conn
		for i := 0; i < MaxConnsPerDevice; i++ {
			conn := connectDevice(t, f, "host-1", "device-1")
			conns = append(conns, conn)
		}
		extra, _, err := dialWS(t, f.wsURL("/device"), nil)
		if err != nil {
			t.Fatal(err)
		}
		mustWrite(t, extra, frame{T: "hello", HostID: "host-1", DeviceID: "device-1"})
		errFrame := expectType(t, extra, "error")
		if errFrame.Code != string(CodeDeviceLimit) {
			t.Fatalf("device limit code = %q", errFrame.Code)
		}
		_ = conns
		_ = host
	})

	t.Run("host-devices", func(t *testing.T) {
		f := newFixture(t, nil)
		host := connectHost(t, f, "host-1")
		for i := 0; i < MaxDevicesPerHost; i++ {
			connectDevice(t, f, "host-1", fmt.Sprintf("device-%d", i))
		}
		extra, _, err := dialWS(t, f.wsURL("/device"), nil)
		if err != nil {
			t.Fatal(err)
		}
		mustWrite(t, extra, frame{T: "hello", HostID: "host-1", DeviceID: "device-overflow"})
		errFrame := expectType(t, extra, "error")
		if errFrame.Code != string(CodeHostDeviceLimit) {
			t.Fatalf("host device limit code = %q", errFrame.Code)
		}
		_ = host
	})
}

func TestRelayUnknownHost(t *testing.T) {
	f := newFixture(t, nil)
	device, _, err := dialWS(t, f.wsURL("/device"), nil)
	if err != nil {
		t.Fatal(err)
	}
	mustWrite(t, device, frame{T: "hello", HostID: "missing-host", DeviceID: "device-1"})
	errFrame := expectType(t, device, "error")
	if errFrame.Code != string(CodeHostOffline) {
		t.Fatalf("unknown host code = %q", errFrame.Code)
	}
}

func TestRelayConcurrentRouting(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")

	const devices = 4
	const connsPerDevice = 2
	const perConn = 10
	total := devices * connsPerDevice * perConn

	deviceConns := make([]*websocket.Conn, devices)
	for d := 0; d < devices; d++ {
		deviceConns[d] = connectDevice(t, f, "host-1", fmt.Sprintf("device-%d", d))
	}
	for d := 0; d < devices; d++ {
		for c := 0; c < connsPerDevice; c++ {
			connID := fmt.Sprintf("d%d-c%d", d, c)
			if err := writeFrame(deviceConns[d], frame{T: "open", Conn: connID}); err != nil {
				t.Fatal(err)
			}
		}
	}
	opens := 0
	for opens < devices*connsPerDevice {
		frame, err := readFrame(host)
		if err != nil {
			t.Fatal(err)
		}
		if frame.T == "open" {
			opens++
		}
	}

	hostErr := make(chan error, 1)
	go func() {
		for i := 0; i < total; i++ {
			frame, err := readFrame(host)
			if err != nil {
				hostErr <- err
				return
			}
			if frame.T != "data" {
				hostErr <- fmt.Errorf("host got %s, want data", frame.T)
				return
			}
			if err := writeFrame(host, frame); err != nil {
				hostErr <- err
				return
			}
		}
		hostErr <- nil
	}()

	type expectation struct {
		conn    string
		payload string
	}
	deviceErrs := make(chan error, devices)
	for d := 0; d < devices; d++ {
		go func(d int) {
			expected := make(map[expectation]bool, connsPerDevice*perConn)
			for c := 0; c < connsPerDevice; c++ {
				connID := fmt.Sprintf("d%d-c%d", d, c)
				for i := 0; i < perConn; i++ {
					payload := base64.StdEncoding.EncodeToString([]byte(fmt.Sprintf("d%d-c%d-i%d", d, c, i)))
					expected[expectation{conn: connID, payload: payload}] = true
					if err := writeFrame(deviceConns[d], frame{T: "data", Conn: connID, Payload: payload}); err != nil {
						deviceErrs <- err
						return
					}
				}
			}
			for i := 0; i < connsPerDevice*perConn; i++ {
				frame, err := readFrame(deviceConns[d])
				if err != nil {
					deviceErrs <- err
					return
				}
				key := expectation{conn: frame.Conn, payload: frame.Payload}
				if !expected[key] {
					deviceErrs <- fmt.Errorf("device %d got unexpected echo %+v", d, key)
					return
				}
				delete(expected, key)
			}
			if len(expected) != 0 {
				deviceErrs <- fmt.Errorf("device %d missing %d echoes", d, len(expected))
				return
			}
			deviceErrs <- nil
		}(d)
	}

	if err := <-hostErr; err != nil {
		t.Fatalf("host echo: %v", err)
	}
	for d := 0; d < devices; d++ {
		if err := <-deviceErrs; err != nil {
			t.Fatalf("device: %v", err)
		}
	}
}

func TestRelayShutdownUnblocks(t *testing.T) {
	f := newFixture(t, nil)
	host := connectHost(t, f, "host-1")
	device := connectDevice(t, f, "host-1", "device-1")
	_ = expectType(t, host, "presence")
	mustWrite(t, device, frame{T: "open", Conn: "conn-1"})
	_ = expectType(t, host, "open")

	start := time.Now()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := f.srv.Shutdown(ctx); err != nil {
		t.Fatalf("shutdown: %v", err)
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("shutdown took %v", elapsed)
	}
	done := make(chan error, 2)
	go func() {
		_, err := readFrame(device)
		done <- err
	}()
	go func() {
		_, err := readFrame(host)
		done <- err
	}()
	for i := 0; i < 2; i++ {
		select {
		case err := <-done:
			if err == nil {
				t.Fatal("peer remained readable after shutdown")
			}
		case <-time.After(2 * time.Second):
			t.Fatal("peer read did not unblock after shutdown")
		}
	}
}

func TestRelayStartAndContextShutdown(t *testing.T) {
	srv, err := New(Config{OperatorToken: testToken})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	errCh := make(chan error, 1)
	go func() { errCh <- srv.Start(ctx, "127.0.0.1:0") }()

	deadline := time.Now().Add(3 * time.Second)
	for srv.Addr() == "" && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	addr := srv.Addr()
	if addr == "" {
		cancel()
		t.Fatal("Start did not expose a listener address")
	}
	resp, err := http.Get("http://" + addr + "/healthz")
	if err != nil {
		cancel()
		t.Fatalf("healthz: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		cancel()
		t.Fatalf("healthz status = %d", resp.StatusCode)
	}

	cancel()
	select {
	case err := <-errCh:
		if err != nil {
			t.Fatalf("Start returned %v", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("Start did not return after context cancellation")
	}
}
