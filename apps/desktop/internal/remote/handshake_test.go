package remote

import (
	"context"
	"regexp"
	"testing"
	"time"
)

type acceptResult struct {
	pairing *Pairing
	err     error
}

func newPairingFixture(t *testing.T, now time.Time) (Invite, *InviteStore, *HostPairer, *Identity) {
	t.Helper()
	hostIdentity := mustIdentity(t)
	invite, err := newInviteAt("host-1", hostIdentity, "wss://relay.example/ws", 2*time.Minute, now)
	if err != nil {
		t.Fatalf("new invite: %v", err)
	}
	store := NewInviteStore()
	store.now = func() time.Time { return now }
	if err := store.Add(invite); err != nil {
		t.Fatalf("add invite: %v", err)
	}
	pairer, err := NewHostPairer(hostIdentity, "host-1", store)
	if err != nil {
		t.Fatalf("host pairer: %v", err)
	}
	return invite, store, pairer, hostIdentity
}

func runAccept(ctx context.Context, pairer *HostPairer, transport Transport) <-chan acceptResult {
	out := make(chan acceptResult, 1)
	go func() {
		pairing, err := pairer.Accept(ctx, transport)
		out <- acceptResult{pairing: pairing, err: err}
	}()
	return out
}

func TestPairingSuccess(t *testing.T) {
	now := time.Now()
	invite, store, pairer, hostIdentity := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	result := runAccept(ctx, pairer, hostTransport)
	session, err := ClientPair(ctx, clientTransport, invite, clientIdentity)
	if err != nil {
		t.Fatalf("client pair: %v", err)
	}
	accepted := <-result
	if accepted.err != nil {
		t.Fatalf("host accept: %v", accepted.err)
	}
	if accepted.pairing == nil || accepted.pairing.Session == nil {
		t.Fatal("host did not return a pairing")
	}
	if !regexp.MustCompile(`^[0-9]{6}$`).MatchString(session.Code) {
		t.Fatalf("code %q is not six digits", session.Code)
	}
	if accepted.pairing.Code() != session.Code {
		t.Fatalf("codes differ: host %q client %q", accepted.pairing.Code(), session.Code)
	}
	if accepted.pairing.Session.C2S != session.C2S || accepted.pairing.Session.S2C != session.S2C {
		t.Fatal("session keys differ")
	}
	if accepted.pairing.Session.Transcript != session.Transcript {
		t.Fatal("transcripts differ")
	}
	if accepted.pairing.DeviceID == "" || accepted.pairing.DeviceID != DeviceIDFromKey(clientIdentity.Public().Ed) {
		t.Fatalf("device id = %q", accepted.pairing.DeviceID)
	}
	if accepted.pairing.Session.HostEd != hostIdentity.Public().Ed {
		t.Fatal("host identity not bound to session")
	}
	if _, err := store.Begin(invite.InviteID); !HasCode(err, CodeInviteUsed) {
		t.Fatalf("invite reuse error = %v, want invite_used", err)
	}
}

func TestPairingTamperedClientSignature(t *testing.T) {
	now := time.Now()
	invite, store, pairer, _ := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	tampered := &tamperTransport{Transport: clientTransport, tamper: tamperField("sig")}
	result := runAccept(ctx, pairer, hostTransport)
	clientDone := make(chan error, 1)
	go func() {
		_, err := ClientPair(ctx, tampered, invite, clientIdentity)
		clientDone <- err
	}()
	accepted := <-result
	if !HasCode(accepted.err, CodeHandshakeSignature) {
		t.Fatalf("host error = %v, want handshake_signature", accepted.err)
	}
	cancel()
	<-clientDone
	if _, err := store.Begin(invite.InviteID); !HasCode(err, CodeInviteUsed) {
		t.Fatalf("invite after failed handshake = %v, want invite_used", err)
	}
}

func TestPairingTamperedClientProof(t *testing.T) {
	now := time.Now()
	invite, store, pairer, _ := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	tampered := &tamperTransport{Transport: clientTransport, tamper: tamperField("proof")}
	result := runAccept(ctx, pairer, hostTransport)
	clientDone := make(chan error, 1)
	go func() {
		_, err := ClientPair(ctx, tampered, invite, clientIdentity)
		clientDone <- err
	}()
	accepted := <-result
	if !HasCode(accepted.err, CodeHandshakeProof) {
		t.Fatalf("host error = %v, want handshake_proof", accepted.err)
	}
	<-clientDone
	if _, err := store.Begin(invite.InviteID); !HasCode(err, CodeInviteUsed) {
		t.Fatalf("invite after failed handshake = %v, want invite_used", err)
	}
}

// fakeHost reads C1 and replies with a caller-built S1.
func fakeHost(ctx context.Context, transport MessageTransport, invite Invite, build func() wireS1) error {
	raw, err := transport.Receive(ctx)
	if err != nil {
		return err
	}
	var c1 wireC1
	if err := decodeStrict(raw, &c1); err != nil {
		return err
	}
	return sendWire(transport, build())
}

func TestClientRejectsWrongPinnedHost(t *testing.T) {
	now := time.Now()
	invite, _, _, _ := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	go func() {
		_ = fakeHost(ctx, hostTransport, invite, func() wireS1 {
			wrongEd, _ := randomKey()
			wrongX, _ := randomKey()
			_, eph, _ := newX25519()
			ns, _ := randomKey()
			return wireS1{
				T: "s1", HostEd: wrongEd, HostX: wrongX, Eph: eph,
				NS: encodeBase64URL(ns[:]), Sig: encodeBase64URL(make([]byte, 64)),
				Proof: encodeBase64URL(make([]byte, 16)),
			}
		})
	}()
	_, err := ClientPair(ctx, clientTransport, invite, clientIdentity)
	if !HasCode(err, CodeHandshakeIdentity) {
		t.Fatalf("client error = %v, want handshake_identity", err)
	}
}

func TestClientRejectsLowOrderHostKey(t *testing.T) {
	now := time.Now()
	invite, _, _, _ := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	go func() {
		_ = fakeHost(ctx, hostTransport, invite, func() wireS1 {
			ns, _ := randomKey()
			return wireS1{
				T: "s1", HostEd: invite.HostEd, HostX: invite.HostX, Eph: Key{},
				NS: encodeBase64URL(ns[:]), Sig: encodeBase64URL(make([]byte, 64)),
				Proof: encodeBase64URL(make([]byte, 16)),
			}
		})
	}()
	_, err := ClientPair(ctx, clientTransport, invite, clientIdentity)
	if !HasCode(err, CodeHandshakeKey) {
		t.Fatalf("client error = %v, want handshake_key", err)
	}
}

func TestClientRejectsHostSignature(t *testing.T) {
	now := time.Now()
	invite, _, _, _ := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	go func() {
		_ = fakeHost(ctx, hostTransport, invite, func() wireS1 {
			_, eph, _ := newX25519()
			ns, _ := randomKey()
			return wireS1{
				T: "s1", HostEd: invite.HostEd, HostX: invite.HostX, Eph: eph,
				NS: encodeBase64URL(ns[:]), Sig: encodeBase64URL(make([]byte, 64)),
				Proof: encodeBase64URL(make([]byte, 16)),
			}
		})
	}()
	_, err := ClientPair(ctx, clientTransport, invite, clientIdentity)
	if !HasCode(err, CodeHandshakeSignature) {
		t.Fatalf("client error = %v, want handshake_signature", err)
	}
}

func TestHostRejectsLowOrderClientKey(t *testing.T) {
	now := time.Now()
	invite, store, pairer, _ := newPairingFixture(t, now)
	clientIdentity := mustIdentity(t)
	clientTransport, hostTransport := transportPair(8)
	ctx, cancel := testContext(t)
	defer cancel()

	nc, _ := randomKey()
	c1 := wireC1{
		T:        "c1",
		InviteID: invite.InviteID,
		Ed:       clientIdentity.Public().Ed,
		X:        clientIdentity.Public().X,
		Eph:      Key{},
		NC:       encodeBase64URL(nc[:]),
		Sig:      encodeBase64URL(make([]byte, 64)),
	}
	if err := sendWire(clientTransport, c1); err != nil {
		t.Fatal(err)
	}
	_, err := pairer.Accept(ctx, hostTransport)
	if !HasCode(err, CodeHandshakeKey) {
		t.Fatalf("host error = %v, want handshake_key", err)
	}
	if _, err := store.Begin(invite.InviteID); !HasCode(err, CodeInviteUsed) {
		t.Fatalf("invite after failed handshake = %v, want invite_used", err)
	}
}
