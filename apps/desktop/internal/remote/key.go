package remote

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
)

// Key is a 32-byte public or symmetric key. Its JSON form is unpadded
// base64url; padded base64url is accepted when decoding.
type Key [32]byte

// String returns the canonical unpadded base64url encoding.
func (k Key) String() string { return base64.RawURLEncoding.EncodeToString(k[:]) }

// IsZero reports whether every byte is zero.
func (k Key) IsZero() bool {
	var zero Key
	return k == zero
}

// MarshalJSON implements json.Marshaler.
func (k Key) MarshalJSON() ([]byte, error) { return json.Marshal(k.String()) }

// UnmarshalJSON implements json.Unmarshaler.
func (k *Key) UnmarshalJSON(data []byte) error {
	var text string
	if err := json.Unmarshal(data, &text); err != nil {
		return errors.New("remote: key must be a base64url string")
	}
	key, err := decodeKey(text)
	if err != nil {
		return err
	}
	*k = key
	return nil
}

func decodeKey(text string) (Key, error) {
	var key Key
	raw, err := decodeBase64URL(text)
	if err != nil || len(raw) != len(key) {
		return key, errors.New("remote: key must be 32 base64url bytes")
	}
	copy(key[:], raw)
	zero(raw)
	return key, nil
}

// decodeBase64URL accepts canonical unpadded base64url and the padded form
// produced by some platform libraries. It never echoes its input in errors.
func decodeBase64URL(text string) ([]byte, error) {
	if raw, err := base64.RawURLEncoding.DecodeString(text); err == nil {
		return raw, nil
	}
	return base64.URLEncoding.DecodeString(text)
}

func encodeBase64URL(raw []byte) string { return base64.RawURLEncoding.EncodeToString(raw) }

func keyFingerprint(k Key) string {
	sum := sha256.Sum256(k[:])
	return encodeBase64URL(sum[:16])
}

func zero(buf []byte) {
	for i := range buf {
		buf[i] = 0
	}
}
