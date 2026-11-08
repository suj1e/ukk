package remote

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"time"
)

const resumeVersion = "ukk-resume-v1"

const (
	proofLabelResumeHost   = "ukk-r2-proof"
	proofLabelResumeDevice = "ukk-r3-proof"
)

// DeviceRecord is the persisted trust record used by resume. Ed and X pin the
// current key generation; Generation is non-zero and increases on rotation or
// revocation. ExpiresAt, when non-zero, is the latest live grant expiry and is
// checked by HostResume. Per-scope grant expiry is still enforced live by
// Registry.Authorize.
type DeviceRecord struct {
	ID         string
	Ed         Key
	X          Key
	Revoked    bool
	Generation uint32
	ExpiresAt  time.Time
}

// DeviceLookup resolves a device by its Ed25519 key. Implementations must
// return false for unknown devices.
type DeviceLookup interface {
	LookupDevice(ed Key) (DeviceRecord, bool)
}

// DeviceLookupFunc adapts a function to DeviceLookup.
type DeviceLookupFunc func(Key) (DeviceRecord, bool)

// LookupDevice implements DeviceLookup.
func (f DeviceLookupFunc) LookupDevice(ed Key) (DeviceRecord, bool) { return f(ed) }

type wireR1 struct {
	T        string `json:"t"`
	HostID   string `json:"host_id"`
	DeviceEd Key    `json:"device_ed"`
	DeviceX  Key    `json:"device_x"`
	Eph      Key    `json:"eph"`
	NC       string `json:"nc"`
	Sig      string `json:"sig"`
}

type wireR2 struct {
	T      string `json:"t"`
	HostEd Key    `json:"host_ed"`
	HostX  Key    `json:"host_x"`
	Eph    Key    `json:"eph"`
	NS     string `json:"ns"`
	Sig    string `json:"sig"`
	Proof  string `json:"proof"`
}

type wireR3 struct {
	T     string `json:"t"`
	Proof string `json:"proof"`
}

// buildResumePrefix hashes the fields the device can sign before it knows the
// host ephemeral. Identifiers are UTF-8 bytes and keys/nonces are raw bytes.
func buildResumePrefix(hostID string, deviceEd, deviceX, ephClient, nc Key) Key {
	h := sha256.New()
	h.Write([]byte(resumeVersion))
	h.Write([]byte(hostID))
	h.Write(deviceEd[:])
	h.Write(deviceX[:])
	h.Write(ephClient[:])
	h.Write(nc[:])
	var out Key
	copy(out[:], h.Sum(nil))
	return out
}

// buildResumeTranscript hashes the full resume transcript.
func buildResumeTranscript(hostID string, deviceEd, deviceX, ephHost, ephClient, nc, ns Key) Key {
	h := sha256.New()
	h.Write([]byte(resumeVersion))
	h.Write([]byte(hostID))
	h.Write(deviceEd[:])
	h.Write(deviceX[:])
	h.Write(ephHost[:])
	h.Write(ephClient[:])
	h.Write(nc[:])
	h.Write(ns[:])
	var out Key
	copy(out[:], h.Sum(nil))
	return out
}

// ClientResume re-authenticates a previously paired device without an invite.
// Every connection uses a fresh client ephemeral and nonce, so channel keys are
// never reused and a captured R1 cannot be replayed successfully.
func ClientResume(ctx context.Context, t MessageTransport, hostID string, hostPin PublicBundle, id *Identity) (*Session, error) {
	if id == nil {
		return nil, newError(CodeHandshakeIdentity, "identity required")
	}
	if !validIdentifier(hostID) {
		return nil, newError(CodeHandshakeIdentity, "invalid host id")
	}
	if hostPin.Ed.IsZero() {
		return nil, newError(CodeHandshakeIdentity, "pinned host signing key required")
	}
	if err := checkX25519Public(hostPin.X); err != nil {
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
	prefix := buildResumePrefix(hostID, id.edPublic, id.xPublic, ephPub, nc)
	sig := id.Sign(prefix[:])
	if len(sig) != ed25519.SignatureSize {
		return nil, newError(CodeHandshakeIdentity, "signing failed")
	}
	r1 := wireR1{
		T:        "r1",
		HostID:   hostID,
		DeviceEd: id.edPublic,
		DeviceX:  id.xPublic,
		Eph:      ephPub,
		NC:       encodeBase64URL(nc[:]),
		Sig:      encodeBase64URL(sig),
	}
	if err := sendWire(t, r1); err != nil {
		return nil, err
	}

	raw, err := receiveWire(ctx, t)
	if err != nil {
		return nil, err
	}
	var r2 wireR2
	if err := decodeStrict(raw, &r2); err != nil || r2.T != "r2" {
		return nil, newError(CodeHandshakeMalformed, "invalid R2 message")
	}
	if !keysEqual(r2.HostEd, hostPin.Ed) || !keysEqual(r2.HostX, hostPin.X) {
		return nil, newError(CodeHandshakeIdentity, "host key does not match pinned identity")
	}
	if err := checkX25519Public(r2.Eph); err != nil {
		return nil, err
	}
	nsBytes, err := decodeFixed(r2.NS, 32)
	if err != nil {
		return nil, err
	}
	var ns Key
	copy(ns[:], nsBytes)
	zero(nsBytes)
	hostSig, err := decodeFixed(r2.Sig, ed25519.SignatureSize)
	if err != nil {
		return nil, err
	}
	proof, err := decodeFixed(r2.Proof, 16)
	if err != nil {
		return nil, err
	}

	transcript := buildResumeTranscript(hostID, id.edPublic, id.xPublic, r2.Eph, ephPub, nc, ns)
	if !ed25519.Verify(hostPin.Ed[:], transcript[:], hostSig) {
		return nil, newError(CodeHandshakeSignature, "host resume signature invalid")
	}

	shared1, err := x25519(ephPriv, r2.Eph)
	if err != nil {
		return nil, err
	}
	shared2, err := x25519(ephPriv, hostPin.X)
	if err != nil {
		return nil, err
	}
	shared3, err := x25519(id.xPrivate, r2.Eph)
	if err != nil {
		return nil, err
	}
	ikm := make([]byte, 0, 3*32)
	ikm = append(ikm, shared1[:]...)
	ikm = append(ikm, shared2[:]...)
	ikm = append(ikm, shared3[:]...)
	keys, err := deriveKeys(transcript, ikm)
	zero(ikm)
	if err != nil {
		return nil, err
	}
	if err := openProof(keys.confirm, transcript, proofLabelResumeHost, proof); err != nil {
		return nil, err
	}

	proof3, err := sealProof(keys.confirm, transcript, proofLabelResumeDevice)
	if err != nil {
		return nil, err
	}
	if err := sendWire(t, wireR3{T: "r3", Proof: encodeBase64URL(proof3)}); err != nil {
		return nil, err
	}
	return &Session{
		C2S:        keys.c2s,
		S2C:        keys.s2c,
		Transcript: transcript,
		HostEd:     hostPin.Ed,
		HostX:      hostPin.X,
		ClientEd:   id.edPublic,
		ClientX:    id.xPublic,
		ExpireMS:   0,
	}, nil
}

// HostResume authenticates a previously paired device. The session is installed
// only after R3 verifies; failures do not consume any bootstrap credential.
//
// R1 carries no generation field, so the persisted key set is the generation
// boundary: lookup is keyed by device_ed and must return false for unknown
// devices and for records whose current generation the caller no longer
// accepts. HostResume additionally rejects a zero generation, a record whose Ed
// key differs from R1, and a stale static X key (the observable result of key
// rotation). ExpiresAt, when set, rejects a device whose trust has lapsed;
// per-scope grant expiry remains a live Registry.Authorize check.
func HostResume(ctx context.Context, t MessageTransport, hostID string, host *Identity, lookup func(Key) (DeviceRecord, bool)) (*Session, string, error) {
	if host == nil {
		return nil, "", newError(CodeHandshakeIdentity, "host identity required")
	}
	if !validIdentifier(hostID) {
		return nil, "", newError(CodeHandshakeIdentity, "invalid host id")
	}
	if lookup == nil {
		return nil, "", newError(CodeHandshakeIdentity, "device lookup required")
	}

	raw, err := receiveWire(ctx, t)
	if err != nil {
		return nil, "", err
	}
	var r1 wireR1
	if err := decodeStrict(raw, &r1); err != nil || r1.T != "r1" {
		return nil, "", newError(CodeHandshakeMalformed, "invalid R1 message")
	}
	if r1.HostID != hostID {
		return nil, "", newError(CodeHandshakeIdentity, "resume names another host")
	}
	if r1.DeviceEd.IsZero() {
		return nil, "", newError(CodeHandshakeKey, "device signing key required")
	}
	if err := checkX25519Public(r1.DeviceX); err != nil {
		return nil, "", err
	}
	if err := checkX25519Public(r1.Eph); err != nil {
		return nil, "", err
	}
	ncBytes, err := decodeFixed(r1.NC, 32)
	if err != nil {
		return nil, "", err
	}
	var nc Key
	copy(nc[:], ncBytes)
	zero(ncBytes)
	sig, err := decodeFixed(r1.Sig, ed25519.SignatureSize)
	if err != nil {
		return nil, "", err
	}

	record, ok := lookup(r1.DeviceEd)
	if !ok {
		return nil, "", newError(CodeUnknownDevice, "unknown device")
	}
	if !validIdentifier(record.ID) {
		return nil, "", newError(CodeUnknownDevice, "device record has no id")
	}
	if record.Revoked {
		return nil, "", newError(CodeDeviceRevoked, "device revoked")
	}
	if record.Generation == 0 {
		return nil, "", newError(CodeStaleIdentity, "device generation missing")
	}
	if record.Ed != r1.DeviceEd {
		return nil, "", newError(CodeHandshakeIdentity, "device record key mismatch")
	}
	if record.X != r1.DeviceX {
		return nil, "", newError(CodeStaleIdentity, "device key generation is stale")
	}
	if !record.ExpiresAt.IsZero() && !time.Now().Before(record.ExpiresAt) {
		return nil, "", newError(CodeGrantExpired, "device grant expired")
	}

	prefix := buildResumePrefix(hostID, r1.DeviceEd, r1.DeviceX, r1.Eph, nc)
	if !ed25519.Verify(r1.DeviceEd[:], prefix[:], sig) {
		return nil, "", newError(CodeHandshakeSignature, "device resume signature invalid")
	}

	hostEphPriv, hostEphPub, err := newX25519()
	if err != nil {
		return nil, "", err
	}
	ns, err := randomKey()
	if err != nil {
		return nil, "", err
	}
	transcript := buildResumeTranscript(hostID, r1.DeviceEd, r1.DeviceX, hostEphPub, r1.Eph, nc, ns)

	shared1, err := x25519(hostEphPriv, r1.Eph)
	if err != nil {
		return nil, "", err
	}
	shared2, err := x25519(host.xPrivate, r1.Eph)
	if err != nil {
		return nil, "", err
	}
	shared3, err := x25519(hostEphPriv, r1.DeviceX)
	if err != nil {
		return nil, "", err
	}
	ikm := make([]byte, 0, 3*32)
	ikm = append(ikm, shared1[:]...)
	ikm = append(ikm, shared2[:]...)
	ikm = append(ikm, shared3[:]...)
	keys, err := deriveKeys(transcript, ikm)
	zero(ikm)
	if err != nil {
		return nil, "", err
	}

	hostSig := ed25519.Sign(host.edPrivate, transcript[:])
	proof, err := sealProof(keys.confirm, transcript, proofLabelResumeHost)
	if err != nil {
		return nil, "", err
	}
	r2 := wireR2{
		T:      "r2",
		HostEd: host.edPublic,
		HostX:  host.xPublic,
		Eph:    hostEphPub,
		NS:     encodeBase64URL(ns[:]),
		Sig:    encodeBase64URL(hostSig),
		Proof:  encodeBase64URL(proof),
	}
	if err := sendWire(t, r2); err != nil {
		return nil, "", err
	}

	raw, err = receiveWire(ctx, t)
	if err != nil {
		return nil, "", err
	}
	var r3 wireR3
	if err := decodeStrict(raw, &r3); err != nil || r3.T != "r3" {
		return nil, "", newError(CodeHandshakeMalformed, "invalid R3 message")
	}
	proof3, err := decodeFixed(r3.Proof, 16)
	if err != nil {
		return nil, "", err
	}
	if err := openProof(keys.confirm, transcript, proofLabelResumeDevice, proof3); err != nil {
		return nil, "", err
	}
	session := &Session{
		C2S:        keys.c2s,
		S2C:        keys.s2c,
		Transcript: transcript,
		HostEd:     host.edPublic,
		HostX:      host.xPublic,
		ClientEd:   r1.DeviceEd,
		ClientX:    r1.DeviceX,
		ExpireMS:   0,
	}
	return session, record.ID, nil
}
