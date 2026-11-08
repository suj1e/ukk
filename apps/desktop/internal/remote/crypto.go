package remote

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"io"

	"golang.org/x/crypto/chacha20poly1305"
	"golang.org/x/crypto/curve25519"
	"golang.org/x/crypto/hkdf"
)

const (
	handshakeVersion = "ukk-handshake-v1"
	hkdfSaltLabel    = "ukk-salt-v1"
	hkdfKeysInfo     = "ukk-confirm|ukk-c2s|ukk-s2c"
	hkdfCodeInfo     = "ukk-code"

	proofLabelServer = "ukk-s1-proof"
	proofLabelClient = "ukk-c2-proof"
)

// randomKey returns 32 cryptographically random bytes.
func randomKey() (Key, error) {
	var key Key
	if _, err := io.ReadFull(rand.Reader, key[:]); err != nil {
		return key, wrapError(CodeHandshakeKey, "random key", err)
	}
	return key, nil
}

// newX25519 returns a fresh X25519 key pair.
func newX25519() (priv, pub Key, err error) {
	priv, err = randomKey()
	if err != nil {
		return Key{}, Key{}, err
	}
	pub, err = x25519Public(priv)
	if err != nil {
		return Key{}, Key{}, err
	}
	return priv, pub, nil
}

// checkX25519Public rejects zero and low-order public keys before any key
// agreement. curve25519.X25519 performs the low-order check for us.
func checkX25519Public(pub Key) error {
	if pub.IsZero() {
		return newError(CodeHandshakeKey, "zero X25519 public key")
	}
	var probe [32]byte
	probe[0] = 9
	probe[31] = 0x40
	shared, err := curve25519.X25519(probe[:], pub[:])
	if err != nil {
		return wrapError(CodeHandshakeKey, "low-order X25519 public key", err)
	}
	defer zero(shared)
	var zeroKey Key
	if len(shared) != len(zeroKey) {
		return newError(CodeHandshakeKey, "invalid X25519 public key")
	}
	copy(zeroKey[:], shared)
	if zeroKey.IsZero() {
		return newError(CodeHandshakeKey, "non-contributory X25519 public key")
	}
	return nil
}

func x25519Public(priv Key) (Key, error) {
	raw, err := curve25519.X25519(priv[:], curve25519.Basepoint)
	if err != nil {
		return Key{}, wrapError(CodeHandshakeKey, "derive public key", err)
	}
	var pub Key
	copy(pub[:], raw)
	zero(raw)
	return pub, nil
}

// x25519 computes a shared secret and rejects low-order or non-contributory
// results. Go's X25519 already refuses known low-order points; the explicit
// zero check covers any other all-zero outcome.
func x25519(priv, pub Key) (Key, error) {
	if priv.IsZero() || pub.IsZero() {
		return Key{}, newError(CodeHandshakeKey, "zero X25519 key")
	}
	raw, err := curve25519.X25519(priv[:], pub[:])
	if err != nil {
		return Key{}, wrapError(CodeHandshakeKey, "invalid X25519 public key", err)
	}
	var shared Key
	copy(shared[:], raw)
	zero(raw)
	if shared.IsZero() {
		return Key{}, newError(CodeHandshakeKey, "non-contributory X25519 result")
	}
	return shared, nil
}

// buildTranscript hashes the canonical handshake prologue. Identifiers are
// UTF-8 bytes, keys raw bytes and expiry 8-byte big-endian milliseconds.
func buildTranscript(hostID, inviteID string, hostEd, hostX, clientEd, clientX, ephHost, ephClient Key, expireMS int64) Key {
	h := sha256.New()
	h.Write([]byte(handshakeVersion))
	h.Write([]byte(hostID))
	h.Write([]byte(inviteID))
	h.Write(hostEd[:])
	h.Write(hostX[:])
	h.Write(clientEd[:])
	h.Write(clientX[:])
	h.Write(ephHost[:])
	h.Write(ephClient[:])
	var expiry [8]byte
	binary.BigEndian.PutUint64(expiry[:], uint64(expireMS))
	h.Write(expiry[:])
	var out Key
	copy(out[:], h.Sum(nil))
	return out
}

// buildClientPrefix hashes the transcript prefix the client can compute before
// S1 supplies the host ephemeral. It is used only for C1's signature.
func buildClientPrefix(hostID, inviteID string, hostEd, hostX, clientEd, clientX, ephClient Key, expireMS int64) Key {
	h := sha256.New()
	h.Write([]byte(handshakeVersion))
	h.Write([]byte(hostID))
	h.Write([]byte(inviteID))
	h.Write(hostEd[:])
	h.Write(hostX[:])
	h.Write(clientEd[:])
	h.Write(clientX[:])
	h.Write(ephClient[:])
	var expiry [8]byte
	binary.BigEndian.PutUint64(expiry[:], uint64(expireMS))
	h.Write(expiry[:])
	var out Key
	copy(out[:], h.Sum(nil))
	return out
}

type derivedKeys struct {
	confirm Key
	c2s     Key
	s2c     Key
	code    string
}

// deriveKeys runs the two HKDF expansions. ikm is consumed but not modified.
func deriveKeys(transcript Key, ikm []byte) (derivedKeys, error) {
	saltInput := make([]byte, 0, len(hkdfSaltLabel)+len(transcript))
	saltInput = append(saltInput, hkdfSaltLabel...)
	saltInput = append(saltInput, transcript[:]...)
	salt := sha256.Sum256(saltInput)

	out := make([]byte, 96)
	if _, err := io.ReadFull(hkdf.New(sha256.New, ikm, salt[:], []byte(hkdfKeysInfo)), out); err != nil {
		return derivedKeys{}, wrapError(CodeHandshakeKey, "derive session keys", err)
	}
	var keys derivedKeys
	copy(keys.confirm[:], out[0:32])
	copy(keys.c2s[:], out[32:64])
	copy(keys.s2c[:], out[64:96])
	zero(out)

	var codeBytes [4]byte
	if _, err := io.ReadFull(hkdf.New(sha256.New, ikm, salt[:], []byte(hkdfCodeInfo)), codeBytes[:]); err != nil {
		return derivedKeys{}, wrapError(CodeHandshakeKey, "derive pairing code", err)
	}
	code := binary.BigEndian.Uint32(codeBytes[:]) % 1_000_000
	keys.code = fmt.Sprintf("%06d", code)
	return keys, nil
}

// proofNonce derives the 12-byte AEAD nonce for a proof label.
func proofNonce(transcript Key, label string) [12]byte {
	h := sha256.New()
	h.Write(transcript[:])
	h.Write([]byte(label))
	sum := h.Sum(nil)
	var nonce [12]byte
	copy(nonce[:], sum[:12])
	zero(sum)
	return nonce
}

// sealProof produces the 16-byte AEAD tag with an empty plaintext.
func sealProof(key Key, transcript Key, label string) ([]byte, error) {
	aead, err := chacha20poly1305.New(key[:])
	if err != nil {
		return nil, wrapError(CodeHandshakeKey, "cipher", err)
	}
	nonce := proofNonce(transcript, label)
	return aead.Seal(nil, nonce[:], nil, transcript[:]), nil
}

// openProof verifies a proof tag in constant time.
func openProof(key Key, transcript Key, label string, proof []byte) error {
	if len(proof) != chacha20poly1305.Overhead {
		return newError(CodeHandshakeProof, "proof has wrong length")
	}
	aead, err := chacha20poly1305.New(key[:])
	if err != nil {
		return wrapError(CodeHandshakeKey, "cipher", err)
	}
	nonce := proofNonce(transcript, label)
	plain, err := aead.Open(nil, nonce[:], proof, transcript[:])
	if err != nil {
		return wrapError(CodeHandshakeProof, "proof verification failed", err)
	}
	zero(plain)
	return nil
}
