package remote

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/json"
	"fmt"
	"time"
)

const maxHandshakeBytes = 4096

// MessageTransport is an ordered, reliable message transport used for the
// pairing handshake. Each Send delivers exactly one message and Receive returns
// messages in send order.
type MessageTransport interface {
	Send([]byte) error
	Receive(context.Context) ([]byte, error)
}

// Session is the result of a successful pairing handshake. Code is the
// six-digit confirmation code; C2S and S2C are the directional envelope keys.
type Session struct {
	Code       string
	C2S        Key
	S2C        Key
	Transcript Key
	HostEd     Key
	HostX      Key
	ClientEd   Key
	ClientX    Key
	ExpireMS   int64
}

// String never prints keys or the confirmation code.
func (s *Session) String() string {
	if s == nil {
		return "remote.Session{<nil>}"
	}
	return fmt.Sprintf("remote.Session{client:%s host:%s}",
		keyFingerprint(s.ClientEd), keyFingerprint(s.HostEd))
}

// Pairing is a host-side successful pairing.
type Pairing struct {
	DeviceID string
	Ed       Key
	X        Key
	Session  *Session
}

// Code returns the six-digit confirmation code.
func (p *Pairing) Code() string {
	if p == nil || p.Session == nil {
		return ""
	}
	return p.Session.Code
}

// DeviceIDFromKey derives the stable device identifier from its Ed25519 key.
func DeviceIDFromKey(ed Key) string {
	sum := sha256.Sum256(ed[:])
	return "dev_" + encodeBase64URL(sum[:])
}

type wireC1 struct {
	T        string `json:"t"`
	InviteID string `json:"invite_id"`
	Ed       Key    `json:"ed"`
	X        Key    `json:"x"`
	Eph      Key    `json:"eph"`
	NC       string `json:"nc"`
	Sig      string `json:"sig"`
}

type wireS1 struct {
	T      string `json:"t"`
	HostEd Key    `json:"host_ed"`
	HostX  Key    `json:"host_x"`
	Eph    Key    `json:"eph"`
	NS     string `json:"ns"`
	Sig    string `json:"sig"`
	Proof  string `json:"proof"`
}

type wireC2 struct {
	T     string `json:"t"`
	Proof string `json:"proof"`
}

func sendWire(t MessageTransport, v any) error {
	if t == nil {
		return newError(CodeTransport, "transport required")
	}
	raw, err := json.Marshal(v)
	if err != nil {
		return wrapError(CodeHandshakeMalformed, "encode handshake message", err)
	}
	if len(raw) > maxHandshakeBytes {
		return newError(CodeHandshakeMalformed, "handshake message exceeds limit")
	}
	if err := t.Send(raw); err != nil {
		return wrapError(CodeTransport, "send handshake message", err)
	}
	return nil
}

func receiveWire(ctx context.Context, t MessageTransport) ([]byte, error) {
	if t == nil {
		return nil, newError(CodeTransport, "transport required")
	}
	raw, err := t.Receive(ctx)
	if err != nil {
		return nil, wrapError(CodeTransport, "receive handshake message", err)
	}
	if len(raw) == 0 || len(raw) > maxHandshakeBytes {
		return nil, newError(CodeHandshakeMalformed, "handshake message size out of range")
	}
	return raw, nil
}

func decodeFixed(text string, size int) ([]byte, error) {
	raw, err := decodeBase64URL(text)
	if err != nil || len(raw) != size {
		return nil, newError(CodeHandshakeMalformed, "handshake field has wrong size")
	}
	return raw, nil
}

func keysEqual(a, b Key) bool {
	return subtle.ConstantTimeCompare(a[:], b[:]) == 1
}

// ClientPair performs the client half of the pairing handshake.
func ClientPair(ctx context.Context, t MessageTransport, invite Invite, id *Identity) (*Session, error) {
	if id == nil {
		return nil, newError(CodeHandshakeIdentity, "identity required")
	}
	if err := invite.Validate(time.Now()); err != nil {
		return nil, err
	}
	ephPriv, ephPub, err := newX25519()
	if err != nil {
		return nil, err
	}
	nc, err := randomKey()
	if err != nil {
		return nil, err
	}
	return clientPair(ctx, t, invite, id, ephPriv, ephPub, nc)
}

func clientPair(ctx context.Context, t MessageTransport, invite Invite, id *Identity, ephPriv, ephPub, nc Key) (*Session, error) {
	defer invite.Secret.Zeroize()

	prefix := buildClientPrefix(invite.HostID, invite.InviteID, invite.HostEd, invite.HostX,
		id.edPublic, id.xPublic, ephPub, invite.ExpireMS)
	sig := id.Sign(prefix[:])
	if len(sig) != ed25519.SignatureSize {
		return nil, newError(CodeHandshakeIdentity, "signing failed")
	}
	c1 := wireC1{
		T:        "c1",
		InviteID: invite.InviteID,
		Ed:       id.edPublic,
		X:        id.xPublic,
		Eph:      ephPub,
		NC:       encodeBase64URL(nc[:]),
		Sig:      encodeBase64URL(sig),
	}
	if err := sendWire(t, c1); err != nil {
		return nil, err
	}

	raw, err := receiveWire(ctx, t)
	if err != nil {
		return nil, err
	}
	var s1 wireS1
	if err := decodeStrict(raw, &s1); err != nil || s1.T != "s1" {
		return nil, newError(CodeHandshakeMalformed, "invalid S1 message")
	}
	if !keysEqual(s1.HostEd, invite.HostEd) || !keysEqual(s1.HostX, invite.HostX) {
		return nil, newError(CodeHandshakeIdentity, "host key does not match pinned invite")
	}
	if err := checkX25519Public(s1.Eph); err != nil {
		return nil, err
	}
	if _, err := decodeFixed(s1.NS, 32); err != nil {
		return nil, err
	}
	hostSig, err := decodeFixed(s1.Sig, ed25519.SignatureSize)
	if err != nil {
		return nil, err
	}
	proof, err := decodeFixed(s1.Proof, 16)
	if err != nil {
		return nil, err
	}

	transcript := buildTranscript(invite.HostID, invite.InviteID, invite.HostEd, invite.HostX,
		id.edPublic, id.xPublic, s1.Eph, ephPub, invite.ExpireMS)
	if !ed25519.Verify(invite.HostEd[:], transcript[:], hostSig) {
		return nil, newError(CodeHandshakeSignature, "host signature invalid")
	}

	shared1, err := x25519(ephPriv, s1.Eph)
	if err != nil {
		return nil, err
	}
	shared2, err := x25519(ephPriv, invite.HostX)
	if err != nil {
		return nil, err
	}
	shared3, err := x25519(id.xPrivate, s1.Eph)
	if err != nil {
		return nil, err
	}
	ikm := make([]byte, 0, 3*32+32)
	ikm = append(ikm, shared1[:]...)
	ikm = append(ikm, shared2[:]...)
	ikm = append(ikm, shared3[:]...)
	ikm = append(ikm, invite.Secret[:]...)
	keys, err := deriveKeys(transcript, ikm)
	zero(ikm)
	if err != nil {
		return nil, err
	}
	if err := openProof(keys.confirm, transcript, proofLabelServer, proof); err != nil {
		return nil, err
	}

	c2proof, err := sealProof(keys.confirm, transcript, proofLabelClient)
	if err != nil {
		return nil, err
	}
	if err := sendWire(t, wireC2{T: "c2", Proof: encodeBase64URL(c2proof)}); err != nil {
		return nil, err
	}
	return &Session{
		Code:       keys.code,
		C2S:        keys.c2s,
		S2C:        keys.s2c,
		Transcript: transcript,
		HostEd:     invite.HostEd,
		HostX:      invite.HostX,
		ClientEd:   id.edPublic,
		ClientX:    id.xPublic,
		ExpireMS:   invite.ExpireMS,
	}, nil
}

// HostPairer accepts pairing attempts for one host identity. Invites are
// claimed from the store before any cryptographic work.
type HostPairer struct {
	identity *Identity
	hostID   string
	invites  *InviteStore
	// Confirm, when set, runs after S1 and before C2 is accepted, so the host
	// user can compare the six-digit code and explicitly allow the pairing.
	// Returning an error denies and burns the invite.
	Confirm func(ctx context.Context, code string) error
}

// NewHostPairer validates its inputs.
func NewHostPairer(id *Identity, hostID string, invites *InviteStore) (*HostPairer, error) {
	if id == nil {
		return nil, newError(CodeHandshakeIdentity, "identity required")
	}
	if !validIdentifier(hostID) {
		return nil, newError(CodeInviteInvalid, "invalid host id")
	}
	if invites == nil {
		return nil, newError(CodeInviteInvalid, "invite store required")
	}
	return &HostPairer{identity: id, hostID: hostID, invites: invites}, nil
}

// Accept performs the host half of the pairing handshake. A failed attempt
// burns the invite.
func (p *HostPairer) Accept(ctx context.Context, t MessageTransport) (*Pairing, error) {
	raw, err := receiveWire(ctx, t)
	if err != nil {
		return nil, err
	}
	var c1 wireC1
	if err := decodeStrict(raw, &c1); err != nil || c1.T != "c1" {
		return nil, newError(CodeHandshakeMalformed, "invalid C1 message")
	}
	if !validIdentifier(c1.InviteID) {
		return nil, newError(CodeHandshakeMalformed, "invalid invite id")
	}

	invite, err := p.invites.Begin(c1.InviteID)
	if err != nil {
		return nil, err
	}
	spent := false
	defer func() {
		if !spent {
			_ = p.invites.Burn(c1.InviteID)
		}
	}()
	defer invite.Secret.Zeroize()

	if invite.HostID != p.hostID {
		return nil, newError(CodeHandshakeIdentity, "invite belongs to another host")
	}
	if !keysEqual(invite.HostEd, p.identity.edPublic) || !keysEqual(invite.HostX, p.identity.xPublic) {
		return nil, newError(CodeHandshakeIdentity, "invite host keys do not match this host")
	}
	if c1.Ed.IsZero() {
		return nil, newError(CodeHandshakeKey, "client signing key required")
	}
	if err := checkX25519Public(c1.X); err != nil {
		return nil, err
	}
	if err := checkX25519Public(c1.Eph); err != nil {
		return nil, err
	}
	if _, err := decodeFixed(c1.NC, 32); err != nil {
		return nil, err
	}
	sig, err := decodeFixed(c1.Sig, ed25519.SignatureSize)
	if err != nil {
		return nil, err
	}
	prefix := buildClientPrefix(invite.HostID, invite.InviteID, invite.HostEd, invite.HostX,
		c1.Ed, c1.X, c1.Eph, invite.ExpireMS)
	if !ed25519.Verify(c1.Ed[:], prefix[:], sig) {
		return nil, newError(CodeHandshakeSignature, "client signature invalid")
	}

	hostEphPriv, hostEphPub, err := newX25519()
	if err != nil {
		return nil, err
	}
	ns, err := randomKey()
	if err != nil {
		return nil, err
	}
	transcript := buildTranscript(invite.HostID, invite.InviteID, invite.HostEd, invite.HostX,
		c1.Ed, c1.X, hostEphPub, c1.Eph, invite.ExpireMS)

	shared1, err := x25519(hostEphPriv, c1.Eph)
	if err != nil {
		return nil, err
	}
	shared2, err := x25519(p.identity.xPrivate, c1.Eph)
	if err != nil {
		return nil, err
	}
	shared3, err := x25519(hostEphPriv, c1.X)
	if err != nil {
		return nil, err
	}
	ikm := make([]byte, 0, 3*32+32)
	ikm = append(ikm, shared1[:]...)
	ikm = append(ikm, shared2[:]...)
	ikm = append(ikm, shared3[:]...)
	ikm = append(ikm, invite.Secret[:]...)
	keys, err := deriveKeys(transcript, ikm)
	zero(ikm)
	if err != nil {
		return nil, err
	}

	hostSig := ed25519.Sign(p.identity.edPrivate, transcript[:])
	proof, err := sealProof(keys.confirm, transcript, proofLabelServer)
	if err != nil {
		return nil, err
	}
	s1 := wireS1{
		T:      "s1",
		HostEd: invite.HostEd,
		HostX:  invite.HostX,
		Eph:    hostEphPub,
		NS:     encodeBase64URL(ns[:]),
		Sig:    encodeBase64URL(hostSig),
		Proof:  encodeBase64URL(proof),
	}
	if err := sendWire(t, s1); err != nil {
		return nil, err
	}
	if p.Confirm != nil {
		if err := p.Confirm(ctx, keys.code); err != nil {
			return nil, err
		}
	}

	raw, err = receiveWire(ctx, t)
	if err != nil {
		return nil, err
	}
	var c2 wireC2
	if err := decodeStrict(raw, &c2); err != nil || c2.T != "c2" {
		return nil, newError(CodeHandshakeMalformed, "invalid C2 message")
	}
	c2proof, err := decodeFixed(c2.Proof, 16)
	if err != nil {
		return nil, err
	}
	if err := openProof(keys.confirm, transcript, proofLabelClient, c2proof); err != nil {
		return nil, err
	}
	if err := p.invites.Complete(c1.InviteID); err != nil {
		return nil, err
	}
	spent = true

	session := &Session{
		Code:       keys.code,
		C2S:        keys.c2s,
		S2C:        keys.s2c,
		Transcript: transcript,
		HostEd:     invite.HostEd,
		HostX:      invite.HostX,
		ClientEd:   c1.Ed,
		ClientX:    c1.X,
		ExpireMS:   invite.ExpireMS,
	}
	return &Pairing{
		DeviceID: DeviceIDFromKey(c1.Ed),
		Ed:       c1.Ed,
		X:        c1.X,
		Session:  session,
	}, nil
}
