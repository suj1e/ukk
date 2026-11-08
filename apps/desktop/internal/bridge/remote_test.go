package bridge

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"log/slog"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/suj1e/ukk/apps/desktop/internal/relay"
	"github.com/suj1e/ukk/apps/desktop/internal/remote"
)

const integrationToken = "0123456789abcdef0123456789abcdef0123456789abcdef"

type deviceWS struct {
	conn     *websocket.Conn
	connID   string
	incoming chan []byte
	done     chan struct{}
}

func dialDevice(ctx context.Context, wsURL, hostID, deviceID string) (*deviceWS, error) {
	dialCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(dialCtx, wsURL+"/device", nil)
	if err != nil {
		return nil, err
	}
	var seed [8]byte
	_, _ = rand.Read(seed[:])
	device := &deviceWS{
		conn: conn, connID: hex.EncodeToString(seed[:]),
		incoming: make(chan []byte, 64), done: make(chan struct{}),
	}
	if err := device.write(relayFrame{T: "hello", HostID: hostID, DeviceID: deviceID}); err != nil {
		conn.Close(websocket.StatusNormalClosure, "")
		return nil, err
	}
	if err := device.write(relayFrame{T: "open", Conn: device.connID}); err != nil {
		conn.Close(websocket.StatusNormalClosure, "")
		return nil, err
	}
	go device.readLoop()
	return device, nil
}

type relayFrame struct {
	T        string `json:"t"`
	HostID   string `json:"host_id,omitempty"`
	DeviceID string `json:"device_id,omitempty"`
	Conn     string `json:"conn,omitempty"`
	Payload  string `json:"payload,omitempty"`
}

func (d *deviceWS) write(frame relayFrame) error {
	raw, err := json.Marshal(frame)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return d.conn.Write(ctx, websocket.MessageText, raw)
}

func (d *deviceWS) readLoop() {
	defer close(d.incoming)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		_, raw, err := d.conn.Read(ctx)
		cancel()
		if err != nil {
			return
		}
		var frame relayFrame
		if err := json.Unmarshal(raw, &frame); err != nil {
			continue
		}
		if frame.T != "data" || frame.Conn != d.connID {
			continue
		}
		payload, err := base64.StdEncoding.DecodeString(frame.Payload)
		if err != nil {
			continue
		}
		select {
		case d.incoming <- payload:
		case <-d.done:
			return
		default:
			return
		}
	}
}

func (d *deviceWS) Send(data []byte) error {
	return d.write(relayFrame{T: "data", Conn: d.connID, Payload: base64.StdEncoding.EncodeToString(data)})
}

func (d *deviceWS) Receive(ctx context.Context) ([]byte, error) {
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-d.done:
		return nil, context.Canceled
	case data, ok := <-d.incoming:
		if !ok {
			return nil, context.Canceled
		}
		return data, nil
	}
}

func (d *deviceWS) Close() error {
	select {
	case <-d.done:
	default:
		close(d.done)
	}
	_ = d.write(relayFrame{T: "close", Conn: d.connID})
	return d.conn.Close(websocket.StatusNormalClosure, "")
}

func waitFor(t *testing.T, timeout time.Duration, what string, fn func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if fn() {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func TestPairResumeAndCommandsThroughRelay(t *testing.T) {
	relayServer, err := relay.New(relay.Config{OperatorToken: integrationToken})
	if err != nil {
		t.Fatal(err)
	}
	httpServer := httptest.NewServer(relayServer.Handler())
	defer httpServer.Close()
	wsURL := "ws" + strings.TrimPrefix(httpServer.URL, "http")

	dir := t.TempDir()
	agents := newFakeAgents()
	server, err := NewServer(ServerOptions{Agents: agents, Workspace: &fakeWorkspace{}})
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	logger := slog.New(slog.NewTextHandler(testWriter{t}, &slog.HandlerOptions{Level: slog.LevelDebug}))
	manager, err := NewRemoteManager(Config{
		HostID: "host-integration", RelayURL: wsURL, OperatorToken: integrationToken,
	}, dir, server, logger)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	manager.Start(ctx)
	waitFor(t, 10*time.Second, "relay connection", func() bool { return manager.Status().RelayConnected })

	invite, err := manager.Invite(ctx)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := remote.ParseInvite([]byte(invite.Payload), time.Now())
	if err != nil {
		t.Fatal(err)
	}
	deviceIdentity, err := remote.GenerateIdentity()
	if err != nil {
		t.Fatal(err)
	}
	deviceID := remote.DeviceIDFromKey(deviceIdentity.Public().Ed)

	device, err := dialDevice(ctx, wsURL, "host-integration", deviceID)
	if err != nil {
		t.Fatal(err)
	}
	defer device.Close()

	type pairResult struct {
		session *remote.Session
		err     error
	}
	pairCh := make(chan pairResult, 1)
	go func() {
		session, err := remote.ClientPair(ctx, device, parsed, deviceIdentity)
		pairCh <- pairResult{session, err}
	}()
	waitFor(t, 15*time.Second, "host confirmation prompt", func() bool {
		return manager.Status().PairingCode != ""
	})
	if err := manager.Confirm(ctx, ConfirmParams{Approve: true}); err != nil {
		t.Fatal(err)
	}
	var session *remote.Session
	select {
	case result := <-pairCh:
		if result.err != nil {
			t.Fatal(result.err)
		}
		session = result.session
	case <-ctx.Done():
		t.Fatal("pairing did not complete")
	}

	conn, err := remote.NewConn(device, session, remote.RoleClient)
	if err != nil {
		t.Fatal(err)
	}
	health := callDevice(t, ctx, conn, "health")
	if !strings.Contains(string(health), `"ok":true`) {
		t.Fatalf("health = %s", health)
	}
	list := callDevice(t, ctx, conn, "agent.list")
	if !strings.Contains(string(list), `"sessions"`) {
		t.Fatalf("agent.list = %s", list)
	}

	// A fresh connection must resume without the invite secret.
	_ = conn.Close()
	_ = device.Close()
	device2, err := dialDevice(ctx, wsURL, "host-integration", deviceID)
	if err != nil {
		t.Fatal(err)
	}
	defer device2.Close()
	resumed, err := remote.ClientResume(ctx, device2, "host-integration", remote.PublicBundle{Ed: parsed.HostEd, X: parsed.HostX}, deviceIdentity)
	if err != nil {
		t.Fatalf("resume: %v", err)
	}
	conn2, err := remote.NewConn(device2, resumed, remote.RoleClient)
	if err != nil {
		t.Fatal(err)
	}
	if got := callDevice(t, ctx, conn2, "health"); !strings.Contains(string(got), `"ok":true`) {
		t.Fatalf("health after resume = %s", got)
	}

	// Revocation denies the next resume.
	if err := manager.Revoke(ctx, deviceID); err != nil {
		t.Fatal(err)
	}
	_ = conn2.Close()
	_ = device2.Close()
	device3, err := dialDevice(ctx, wsURL, "host-integration", deviceID)
	if err != nil {
		// The relay may already refuse a revoked device.
		return
	}
	defer device3.Close()
	if _, err := remote.ClientResume(ctx, device3, "host-integration", remote.PublicBundle{Ed: parsed.HostEd, X: parsed.HostX}, deviceIdentity); err == nil {
		t.Fatal("revoked device resumed")
	}
}

func callDevice(t *testing.T, ctx context.Context, conn *remote.Conn, method string) json.RawMessage {
	t.Helper()
	request, _ := json.Marshal(Request{ID: "r-" + method, Method: method})
	if err := conn.SendEnvelope(0, request); err != nil {
		t.Fatal(err)
	}
	stream, payload, err := conn.ReceiveEnvelope(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if stream != 0 {
		t.Fatalf("stream = %d", stream)
	}
	var response Response
	if err := json.Unmarshal(payload, &response); err != nil {
		t.Fatal(err)
	}
	if response.Error != nil {
		t.Fatalf("%s error = %+v", method, response.Error)
	}
	return response.Result
}

type testWriter struct{ t *testing.T }

func (w testWriter) Write(p []byte) (int, error) {
	w.t.Log(strings.TrimSpace(string(p)))
	return len(p), nil
}
