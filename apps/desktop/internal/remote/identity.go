package remote

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
)

// PublicBundle is the public half of a device identity.
type PublicBundle struct {
	Ed Key `json:"ed"`
	X  Key `json:"x"`
}

// Identity is an Ed25519 signing key (stored as its 32-byte seed) plus an
// X25519 static key. It is persisted as JSON with mode 0600.
type Identity struct {
	edPrivate ed25519.PrivateKey
	xPrivate  Key
	edPublic  Key
	xPublic   Key
}

type identityFile struct {
	V        int `json:"v"`
	EdSeed   Key `json:"ed_seed"`
	XPrivate Key `json:"x_private"`
	EdPublic Key `json:"ed_public"`
	XPublic  Key `json:"x_public"`
}

const identityVersion = 1

// GenerateIdentity creates a fresh signing and static key pair.
func GenerateIdentity() (*Identity, error) {
	var seed Key
	if _, err := io.ReadFull(rand.Reader, seed[:]); err != nil {
		return nil, wrapError(CodeIdentityInvalid, "generate signing seed", err)
	}
	var xPrivate Key
	if _, err := io.ReadFull(rand.Reader, xPrivate[:]); err != nil {
		return nil, wrapError(CodeIdentityInvalid, "generate static key", err)
	}
	id, err := identityFrom(seed, xPrivate)
	zero(seed[:])
	return id, err
}

func identityFrom(seed, xPrivate Key) (*Identity, error) {
	if seed.IsZero() || xPrivate.IsZero() {
		return nil, newError(CodeIdentityInvalid, "seed and static key must be non-zero")
	}
	edPrivate := ed25519.NewKeyFromSeed(seed[:])
	xPublic, err := x25519Public(xPrivate)
	if err != nil {
		return nil, err
	}
	id := &Identity{
		edPrivate: append(ed25519.PrivateKey(nil), edPrivate...),
		xPrivate:  xPrivate,
		xPublic:   xPublic,
	}
	copy(id.edPublic[:], edPrivate.Public().(ed25519.PublicKey))
	zero(edPrivate)
	return id, nil
}

// LoadIdentity reads a persisted identity. Symlinks and files readable by
// group or others are refused rather than repaired.
func LoadIdentity(path string) (*Identity, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, wrapError(CodeIdentityInvalid, "read identity", err)
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
		return nil, newError(CodeIdentityInvalid, "identity must be a regular file")
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		return nil, newError(CodeIdentityInvalid, "identity file must have mode 0600")
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, wrapError(CodeIdentityInvalid, "read identity", err)
	}
	var file identityFile
	if err := decodeStrict(raw, &file); err != nil {
		return nil, newError(CodeIdentityInvalid, "identity is not valid JSON")
	}
	if file.V != identityVersion {
		return nil, newError(CodeIdentityInvalid, "unsupported identity version")
	}
	id, err := identityFrom(file.EdSeed, file.XPrivate)
	if err != nil {
		return nil, err
	}
	file.EdSeed = Key{}
	file.XPrivate = Key{}
	if id.edPublic != file.EdPublic || id.xPublic != file.XPublic {
		id.Zeroize()
		return nil, newError(CodeIdentityInvalid, "identity public keys do not match its private keys")
	}
	return id, nil
}

// SaveIdentity writes the identity atomically with mode 0600.
func SaveIdentity(path string, id *Identity) error {
	if id == nil {
		return newError(CodeIdentityInvalid, "identity required")
	}
	if path == "" {
		return newError(CodeIdentityInvalid, "identity path required")
	}
	var seed Key
	copy(seed[:], id.edPrivate.Seed())
	file := identityFile{
		V:        identityVersion,
		EdSeed:   seed,
		XPrivate: id.xPrivate,
		EdPublic: id.edPublic,
		XPublic:  id.xPublic,
	}
	raw, err := json.Marshal(file)
	zero(seed[:])
	if err != nil {
		return wrapError(CodeIdentityInvalid, "encode identity", err)
	}
	if err := writePrivateFile(path, raw); err != nil {
		return wrapError(CodeIdentityInvalid, "write identity", err)
	}
	return nil
}

// Public returns the public bundle.
func (i *Identity) Public() PublicBundle {
	if i == nil {
		return PublicBundle{}
	}
	return PublicBundle{Ed: i.edPublic, X: i.xPublic}
}

// Sign signs msg with the Ed25519 signing key.
func (i *Identity) Sign(msg []byte) []byte {
	if i == nil || len(i.edPrivate) != ed25519.PrivateKeySize {
		return nil
	}
	return ed25519.Sign(i.edPrivate, msg)
}

// Fingerprint is the unpadded base64url SHA-256 of the Ed25519 public key.
func (i *Identity) Fingerprint() string {
	if i == nil {
		return ""
	}
	sum := sha256.Sum256(i.edPublic[:])
	return encodeBase64URL(sum[:])
}

// Zeroize erases private key material held by the identity.
func (i *Identity) Zeroize() {
	if i == nil {
		return
	}
	zero(i.edPrivate)
	zero(i.xPrivate[:])
}

// writePrivateFile writes raw to path through a same-directory temporary file
// and rename, then forces mode 0600.
func writePrivateFile(path string, raw []byte) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, ".ukk-identity-*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	cleanup := func() {
		_ = tmp.Close()
		_ = os.Remove(tmpName)
	}
	if err := tmp.Chmod(0o600); err != nil {
		cleanup()
		return err
	}
	if _, err := tmp.Write(raw); err != nil {
		cleanup()
		return err
	}
	if err := tmp.Sync(); err != nil {
		cleanup()
		return err
	}
	if err := tmp.Close(); err != nil {
		_ = os.Remove(tmpName)
		return err
	}
	if err := os.Rename(tmpName, path); err != nil {
		_ = os.Remove(tmpName)
		return err
	}
	return os.Chmod(path, 0o600)
}

// decodeStrict decodes exactly one JSON value and rejects unknown fields and
// trailing data. Error text is intentionally generic so payload contents and
// secrets are never reflected.
func decodeStrict(raw []byte, v any) error {
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		return err
	}
	if err := dec.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return errors.New("remote: trailing data")
	}
	return nil
}
