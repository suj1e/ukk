package remote

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"
)

func testSession(t *testing.T) *Session {
	t.Helper()
	c2s, err := randomKey()
	if err != nil {
		t.Fatal(err)
	}
	s2c, err := randomKey()
	if err != nil {
		t.Fatal(err)
	}
	return &Session{C2S: c2s, S2C: s2c}
}

func TestEnvelopeRoundTrip(t *testing.T) {
	session := testSession(t)
	clientTransport, hostTransport := transportPair(64)
	clientConn, err := NewConn(clientTransport, session, RoleClient)
	if err != nil {
		t.Fatal(err)
	}
	hostConn, err := NewConn(hostTransport, session, RoleHost)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := testContext(t)
	defer cancel()

	for i := 0; i < 5; i++ {
		payload := []byte(fmt.Sprintf("client-%d", i))
		if err := clientConn.SendEnvelope(3, payload); err != nil {
			t.Fatalf("send %d: %v", i, err)
		}
		stream, got, err := hostConn.ReceiveEnvelope(ctx)
		if err != nil {
			t.Fatalf("receive %d: %v", i, err)
		}
		if stream != 3 || string(got) != string(payload) {
			t.Fatalf("got stream %d payload %q", stream, got)
		}
	}
	if err := hostConn.SendEnvelope(9, []byte("host-to-client")); err != nil {
		t.Fatal(err)
	}
	stream, got, err := clientConn.ReceiveEnvelope(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if stream != 9 || string(got) != "host-to-client" {
		t.Fatalf("got stream %d payload %q", stream, got)
	}
}

func TestEnvelopeReplayWindow(t *testing.T) {
	session := testSession(t)
	ciph, err := newEnvelopeCipher(session.C2S)
	if err != nil {
		t.Fatal(err)
	}
	seal := func(stream uint32, counter uint64, payload string) []byte {
		raw, err := ciph.seal(stream, counter, []byte(payload))
		if err != nil {
			t.Fatalf("seal: %v", err)
		}
		return raw
	}

	t.Run("duplicate", func(t *testing.T) {
		transport := newMemTransport()
		conn, _ := NewConn(transport, session, RoleHost)
		envelope := seal(1, 1, "one")
		transport.push(envelope)
		transport.push(envelope)
		ctx, cancel := testContext(t)
		defer cancel()
		if _, _, err := conn.ReceiveEnvelope(ctx); err != nil {
			t.Fatalf("first receive: %v", err)
		}
		if _, _, err := conn.ReceiveEnvelope(ctx); !HasCode(err, CodeEnvelopeReplay) {
			t.Fatalf("replay error = %v", err)
		}
	})

	t.Run("beyond-window", func(t *testing.T) {
		transport := newMemTransport()
		conn, _ := NewConn(transport, session, RoleHost)
		transport.push(seal(1, 5000, "high"))
		transport.push(seal(1, 1, "old"))
		ctx, cancel := testContext(t)
		defer cancel()
		if _, _, err := conn.ReceiveEnvelope(ctx); err != nil {
			t.Fatalf("high receive: %v", err)
		}
		if _, _, err := conn.ReceiveEnvelope(ctx); !HasCode(err, CodeEnvelopeReplay) {
			t.Fatalf("old error = %v", err)
		}
	})

	t.Run("out-of-order", func(t *testing.T) {
		transport := newMemTransport()
		conn, _ := NewConn(transport, session, RoleHost)
		transport.push(seal(1, 1, "one"))
		transport.push(seal(1, 3, "three"))
		transport.push(seal(1, 2, "two"))
		ctx, cancel := testContext(t)
		defer cancel()
		for i, want := range []string{"one", "three", "two"} {
			_, got, err := conn.ReceiveEnvelope(ctx)
			if err != nil {
				t.Fatalf("receive %d: %v", i, err)
			}
			if string(got) != want {
				t.Fatalf("receive %d = %q, want %q", i, got, want)
			}
		}
		transport.push(seal(1, 3, "three"))
		if _, _, err := conn.ReceiveEnvelope(ctx); !HasCode(err, CodeEnvelopeReplay) {
			t.Fatalf("duplicate error = %v", err)
		}
	})
}

func TestEnvelopeRejectsTamperingAndOversize(t *testing.T) {
	session := testSession(t)
	ciph, err := newEnvelopeCipher(session.C2S)
	if err != nil {
		t.Fatal(err)
	}
	valid, err := ciph.seal(1, 1, []byte("payload"))
	if err != nil {
		t.Fatal(err)
	}

	receive := func(raw []byte) error {
		transport := newMemTransport()
		transport.push(raw)
		conn, err := NewConn(transport, session, RoleHost)
		if err != nil {
			t.Fatal(err)
		}
		ctx, cancel := testContext(t)
		defer cancel()
		_, _, err = conn.ReceiveEnvelope(ctx)
		return err
	}

	badVersion := append([]byte(nil), valid...)
	badVersion[0] = 2
	if err := receive(badVersion); !HasCode(err, CodeEnvelopeVersion) {
		t.Fatalf("version error = %v", err)
	}
	badStream := append([]byte(nil), valid...)
	badStream[1] ^= 0xff
	if err := receive(badStream); !HasCode(err, CodeEnvelopeAuth) {
		t.Fatalf("stream tamper error = %v", err)
	}
	badCounter := append([]byte(nil), valid...)
	badCounter[12] ^= 0xff
	if err := receive(badCounter); !HasCode(err, CodeEnvelopeAuth) {
		t.Fatalf("counter tamper error = %v", err)
	}
	if err := receive(valid[:8]); !HasCode(err, CodeEnvelopeInvalid) {
		t.Fatalf("short error = %v", err)
	}
	zeroCounter := make([]byte, envelopeHeaderSize+16)
	zeroCounter[0] = EnvelopeVersion
	zeroCounter[4] = 1
	if err := receive(zeroCounter); !HasCode(err, CodeEnvelopeInvalid) {
		t.Fatalf("zero counter error = %v", err)
	}
	oversize := make([]byte, envelopeHeaderSize+16+MaxPlaintext+1)
	oversize[0] = EnvelopeVersion
	oversize[4] = 1
	oversize[12] = 1
	if err := receive(oversize); !HasCode(err, CodeEnvelopeSize) {
		t.Fatalf("oversize error = %v", err)
	}

	// A different session key cannot authenticate the envelope.
	other := testSession(t)
	transport := newMemTransport()
	transport.push(valid)
	conn, _ := NewConn(transport, other, RoleHost)
	ctx, cancel := testContext(t)
	defer cancel()
	if _, _, err := conn.ReceiveEnvelope(ctx); !HasCode(err, CodeEnvelopeAuth) {
		t.Fatalf("wrong key error = %v", err)
	}

	clientTransport, _ := transportPair(2)
	sender, _ := NewConn(clientTransport, session, RoleClient)
	if err := sender.SendEnvelope(1, make([]byte, MaxPlaintext+1)); !HasCode(err, CodeEnvelopeSize) {
		t.Fatalf("send oversize error = %v", err)
	}
}

func TestConnConcurrentSend(t *testing.T) {
	session := testSession(t)
	clientTransport, hostTransport := transportPair(4096)
	clientConn, err := NewConn(clientTransport, session, RoleClient)
	if err != nil {
		t.Fatal(err)
	}
	hostConn, err := NewConn(hostTransport, session, RoleHost)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := testContext(t)
	defer cancel()

	const workers = 16
	const perWorker = 20
	var wg sync.WaitGroup
	errs := make(chan error, workers)
	for worker := 0; worker < workers; worker++ {
		wg.Add(1)
		go func(worker int) {
			defer wg.Done()
			for i := 0; i < perWorker; i++ {
				payload := fmt.Sprintf("%d-%d", worker, i)
				if err := clientConn.SendEnvelope(1, []byte(payload)); err != nil {
					errs <- err
					return
				}
			}
		}(worker)
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatalf("send: %v", err)
	}

	seen := make(map[string]bool, workers*perWorker)
	for i := 0; i < workers*perWorker; i++ {
		stream, payload, err := hostConn.ReceiveEnvelope(ctx)
		if err != nil {
			t.Fatalf("receive %d: %v", i, err)
		}
		if stream != 1 {
			t.Fatalf("stream = %d", stream)
		}
		key := string(payload)
		if seen[key] {
			t.Fatalf("duplicate payload %q", key)
		}
		seen[key] = true
	}
	if len(seen) != workers*perWorker {
		t.Fatalf("received %d payloads, want %d", len(seen), workers*perWorker)
	}
}

func TestConnCloseUnblocksReceive(t *testing.T) {
	session := testSession(t)
	clientTransport, hostTransport := transportPair(4)
	clientConn, _ := NewConn(clientTransport, session, RoleClient)
	hostConn, _ := NewConn(hostTransport, session, RoleHost)

	done := make(chan error, 1)
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_, _, err := hostConn.ReceiveEnvelope(ctx)
		done <- err
	}()
	if err := clientConn.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("receive returned no error after close")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("receive did not unblock after close")
	}
	if !clientConn.Closed() {
		t.Fatal("Closed() = false after Close")
	}
}
