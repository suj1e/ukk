package remote

import (
	"context"
	"encoding/json"
	"testing"
	"time"
)

type resumeHostResult struct {
	session  *Session
	deviceID string
	err      error
}

func runHostResume(ctx context.Context, hostID string, host *Identity, transport Transport, lookup func(Key) (DeviceRecord, bool)) <-chan resumeHostResult {
	out := make(chan resumeHostResult, 1)
	go func() {
		session, deviceID, err := HostResume(ctx, transport, hostID, host, lookup)
		out <- resumeHostResult{session: session, deviceID: deviceID, err: err}
	}()
	return out
}

func resumeFixture(t *testing.T) (*Identity, *Identity, DeviceRecord, func(Key) (DeviceRecord, bool)) {
	t.Helper()
	host := mustIdentity(t)
	device := mustIdentity(t)
	record := DeviceRecord{
		ID:         "device-1",
		Ed:         device.Public().Ed,
		X:          device.Public().X,
		Generation: 1,
	}
	lookup := func(ed Key) (DeviceRecord, bool) {
		if ed == record.Ed {
			return record, true
		}
		return DeviceRecord{}, false
	}
	return host, device, record, lookup
}

func TestResumeSuccess(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	result := runHostResume(ctx, "host-1", host, hostTransport, lookup)
	clientSession, err := ClientResume(ctx, clientTransport, "host-1", host.Public(), device)
	if err != nil {
		t.Fatalf("client resume: %v", err)
	}
	hosted := <-result
	if hosted.err != nil {
		t.Fatalf("host resume: %v", hosted.err)
	}
	if hosted.deviceID != "device-1" {
		t.Fatalf("device id = %q", hosted.deviceID)
	}
	if hosted.session.C2S != clientSession.C2S || hosted.session.S2C != clientSession.S2C {
		t.Fatal("resume session keys differ")
	}
	if hosted.session.Transcript != clientSession.Transcript {
		t.Fatal("resume transcripts differ")
	}
	if clientSession.ExpireMS != 0 {
		t.Fatalf("ExpireMS = %d, want 0", clientSession.ExpireMS)
	}
	if clientSession.Code != "" {
		t.Fatalf("resume code = %q, want empty", clientSession.Code)
	}

	clientConn, err := NewConn(clientTransport, clientSession, RoleClient)
	if err != nil {
		t.Fatal(err)
	}
	hostConn, err := NewConn(hostTransport, hosted.session, RoleHost)
	if err != nil {
		t.Fatal(err)
	}
	if err := clientConn.SendEnvelope(1, []byte("resumed")); err != nil {
		t.Fatal(err)
	}
	stream, plaintext, err := hostConn.ReceiveEnvelope(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if stream != 1 || string(plaintext) != "resumed" {
		t.Fatalf("got stream %d payload %q", stream, plaintext)
	}
}

func TestResumeTamperedR1Signature(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	tampered := &tamperTransport{Transport: clientTransport, tamper: tamperField("sig")}
	result := runHostResume(ctx, "host-1", host, hostTransport, lookup)
	clientDone := make(chan error, 1)
	go func() {
		_, err := ClientResume(ctx, tampered, "host-1", host.Public(), device)
		clientDone <- err
	}()
	hosted := <-result
	if !HasCode(hosted.err, CodeHandshakeSignature) {
		t.Fatalf("host error = %v, want handshake_signature", hosted.err)
	}
	cancel()
	<-clientDone
}

func TestResumeUnknownAndRevokedDevice(t *testing.T) {
	host, device, record, _ := resumeFixture(t)
	unknown := func(Key) (DeviceRecord, bool) { return DeviceRecord{}, false }
	for name, lookup := range map[string]func(Key) (DeviceRecord, bool){
		"unknown": unknown,
		"revoked": func(Key) (DeviceRecord, bool) {
			rec := record
			rec.Revoked = true
			return rec, true
		},
		"expired": func(Key) (DeviceRecord, bool) {
			rec := record
			rec.ExpiresAt = time.Now().Add(-time.Minute)
			return rec, true
		},
		"no-generation": func(Key) (DeviceRecord, bool) {
			rec := record
			rec.Generation = 0
			return rec, true
		},
		"stale-x": func(Key) (DeviceRecord, bool) {
			rec := record
			other, _ := randomKey()
			rec.X = other
			return rec, true
		},
	} {
		t.Run(name, func(t *testing.T) {
			clientTransport, hostTransport := transportPair(8)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			result := runHostResume(ctx, "host-1", host, hostTransport, lookup)
			clientDone := make(chan error, 1)
			go func() {
				_, err := ClientResume(ctx, clientTransport, "host-1", host.Public(), device)
				clientDone <- err
			}()
			hosted := <-result
			want := map[string]Code{
				"unknown":       CodeUnknownDevice,
				"revoked":       CodeDeviceRevoked,
				"expired":       CodeGrantExpired,
				"no-generation": CodeStaleIdentity,
				"stale-x":       CodeStaleIdentity,
			}[name]
			if !HasCode(hosted.err, want) {
				t.Fatalf("host error = %v, want %s", hosted.err, want)
			}
			cancel()
			<-clientDone
		})
	}
}

func TestResumeClientRejectsWrongPinnedHost(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	otherHost := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	result := runHostResume(ctx, "host-1", host, hostTransport, lookup)
	_, err := ClientResume(ctx, clientTransport, "host-1", otherHost.Public(), device)
	if !HasCode(err, CodeHandshakeIdentity) {
		t.Fatalf("client error = %v, want handshake_identity", err)
	}
	cancel()
	<-result
}

func TestResumeTamperedR2Proof(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	tamperedHost := &tamperTransport{Transport: hostTransport, tamper: tamperField("proof")}
	result := runHostResume(ctx, "host-1", host, tamperedHost, lookup)
	_, err := ClientResume(ctx, clientTransport, "host-1", host.Public(), device)
	if !HasCode(err, CodeHandshakeProof) {
		t.Fatalf("client error = %v, want handshake_proof", err)
	}
	cancel()
	<-result
}

func TestResumeTamperedR3Proof(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	tamperedClient := &tamperTransport{Transport: clientTransport, tamper: tamperField("proof")}
	result := runHostResume(ctx, "host-1", host, hostTransport, lookup)
	clientDone := make(chan error, 1)
	go func() {
		_, err := ClientResume(ctx, tamperedClient, "host-1", host.Public(), device)
		clientDone <- err
	}()
	hosted := <-result
	if !HasCode(hosted.err, CodeHandshakeProof) {
		t.Fatalf("host error = %v, want handshake_proof", hosted.err)
	}
	if err := <-clientDone; err != nil {
		t.Fatalf("client error = %v (client cannot observe R3 failure)", err)
	}
}

func TestResumeReplayUsesFreshTranscript(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	recorder := &recordTransport{Transport: clientTransport}
	result := runHostResume(ctx, "host-1", host, hostTransport, lookup)
	first, err := ClientResume(ctx, recorder, "host-1", host.Public(), device)
	if err != nil {
		t.Fatalf("first resume: %v", err)
	}
	hosted := <-result
	if hosted.err != nil {
		t.Fatalf("first host resume: %v", hosted.err)
	}

	var capturedR1 []byte
	for _, frame := range recorder.frames() {
		var probe struct {
			T string `json:"t"`
		}
		if json.Unmarshal(frame, &probe) == nil && probe.T == "r1" {
			capturedR1 = frame
			break
		}
	}
	if capturedR1 == nil {
		t.Fatal("R1 was not captured")
	}

	replayTransport := newMemTransport()
	replayTransport.push(capturedR1)
	replayCtx, replayCancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer replayCancel()
	_, _, replayErr := HostResume(replayCtx, replayTransport, "host-1", host, lookup)
	if replayErr == nil {
		t.Fatal("replayed R1 unexpectedly completed")
	}
	var r1 wireR1
	if err := decodeStrict(capturedR1, &r1); err != nil {
		t.Fatal(err)
	}
	frames := replayTransport.sentFrames()
	if len(frames) != 1 {
		t.Fatalf("replay sent %d frames, want one R2", len(frames))
	}
	var r2 wireR2
	if err := decodeStrict(frames[0], &r2); err != nil {
		t.Fatal(err)
	}
	nc, _ := decodeFixed(r1.NC, 32)
	var ncKey Key
	copy(ncKey[:], nc)
	ns, _ := decodeFixed(r2.NS, 32)
	var nsKey Key
	copy(nsKey[:], ns)
	replayedTranscript := buildResumeTranscript("host-1", r1.DeviceEd, r1.DeviceX, r2.Eph, r1.Eph, ncKey, nsKey)
	if replayedTranscript == first.Transcript {
		t.Fatal("replayed R1 produced the original transcript")
	}
}

func TestResumeRejectsLowOrderKeys(t *testing.T) {
	host, device, _, lookup := resumeFixture(t)
	nc, _ := randomKey()

	for name, mutate := range map[string]func(*wireR1){
		"ephemeral": func(r1 *wireR1) { r1.Eph = Key{} },
		"static":    func(r1 *wireR1) { r1.DeviceX = Key{} },
	} {
		t.Run(name, func(t *testing.T) {
			_, validEph, _ := newX25519()
			r1 := wireR1{
				T:        "r1",
				HostID:   "host-1",
				DeviceEd: device.Public().Ed,
				DeviceX:  device.Public().X,
				Eph:      validEph,
				NC:       encodeBase64URL(nc[:]),
				Sig:      encodeBase64URL(make([]byte, 64)),
			}
			mutate(&r1)
			transport := newMemTransport()
			raw, err := json.Marshal(r1)
			if err != nil {
				t.Fatal(err)
			}
			transport.push(raw)
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			_, _, err = HostResume(ctx, transport, "host-1", host, lookup)
			if !HasCode(err, CodeHandshakeKey) {
				t.Fatalf("error = %v, want handshake_key", err)
			}
		})
	}
}
