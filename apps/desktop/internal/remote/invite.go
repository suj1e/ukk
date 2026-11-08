package remote

import (
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/url"
	"sync"
	"time"
)

const (
	// DefaultInviteTTL is the lifetime used when none is requested.
	DefaultInviteTTL = 2 * time.Minute
	// MaxInviteTTL is the longest accepted invite lifetime.
	MaxInviteTTL = 5 * time.Minute
	// maxInviteBytes bounds an encoded invite.
	maxInviteBytes = 2048
)

// Secret is the 32-byte one-time invite secret. It formats as "[redacted]" and
// can be explicitly zeroized.
type Secret [32]byte

func (s Secret) String() string   { return "[redacted]" }
func (s Secret) GoString() string { return "remote.Secret{[redacted]}" }

// MarshalJSON implements json.Marshaler.
func (s Secret) MarshalJSON() ([]byte, error) {
	return json.Marshal(encodeBase64URL(s[:]))
}

// UnmarshalJSON implements json.Unmarshaler without echoing input.
func (s *Secret) UnmarshalJSON(data []byte) error {
	var text string
	if err := json.Unmarshal(data, &text); err != nil {
		return errors.New("remote: secret must be a base64url string")
	}
	raw, err := decodeBase64URL(text)
	if err != nil || len(raw) != len(s) {
		return errors.New("remote: secret must be 32 bytes")
	}
	copy(s[:], raw)
	zero(raw)
	return nil
}

// Zeroize erases the secret.
func (s *Secret) Zeroize() {
	if s == nil {
		return
	}
	zero(s[:])
}

// Invite is the single-use bootstrap credential encoded in a pairing QR code.
type Invite struct {
	V        int    `json:"v"`
	HostID   string `json:"host_id"`
	HostEd   Key    `json:"host_ed"`
	HostX    Key    `json:"host_x"`
	InviteID string `json:"invite_id"`
	Secret   Secret `json:"secret"`
	ExpireMS int64  `json:"expire_ms"`
	Relay    string `json:"relay"`
}

// String never prints the secret.
func (i Invite) String() string {
	return fmt.Sprintf("remote.Invite{v:%d host:%s id:%s expire_ms:%d relay:%s}",
		i.V, i.HostID, i.InviteID, i.ExpireMS, i.Relay)
}

// GoString never prints the secret.
func (i Invite) GoString() string { return i.String() }

// NewInvite builds an invite for hostID using id's public keys. A zero ttl
// selects DefaultInviteTTL.
func NewInvite(hostID string, id *Identity, relay string, ttl time.Duration) (Invite, error) {
	return newInviteAt(hostID, id, relay, ttl, time.Now())
}

func newInviteAt(hostID string, id *Identity, relay string, ttl time.Duration, now time.Time) (Invite, error) {
	if id == nil {
		return Invite{}, newError(CodeInviteInvalid, "identity required")
	}
	if ttl == 0 {
		ttl = DefaultInviteTTL
	}
	if ttl <= 0 {
		return Invite{}, newError(CodeInviteInvalid, "invite lifetime must be positive")
	}
	if ttl > MaxInviteTTL {
		return Invite{}, newError(CodeInviteInvalid, "invite lifetime exceeds five minutes")
	}
	if !validIdentifier(hostID) {
		return Invite{}, newError(CodeInviteInvalid, "invalid host id")
	}
	if err := validateRelay(relay); err != nil {
		return Invite{}, err
	}
	var inviteID [16]byte
	if _, err := io.ReadFull(rand.Reader, inviteID[:]); err != nil {
		return Invite{}, wrapError(CodeInviteInvalid, "generate invite id", err)
	}
	var secret Secret
	if _, err := io.ReadFull(rand.Reader, secret[:]); err != nil {
		return Invite{}, wrapError(CodeInviteInvalid, "generate invite secret", err)
	}
	pub := id.Public()
	invite := Invite{
		V:        1,
		HostID:   hostID,
		HostEd:   pub.Ed,
		HostX:    pub.X,
		InviteID: encodeBase64URL(inviteID[:]),
		Secret:   secret,
		ExpireMS: now.Add(ttl).UnixMilli(),
		Relay:    relay,
	}
	if err := invite.Validate(now); err != nil {
		secret.Zeroize()
		return Invite{}, err
	}
	return invite, nil
}

// Marshal encodes the invite as canonical JSON.
func (i Invite) Marshal() ([]byte, error) {
	raw, err := json.Marshal(i)
	if err != nil {
		return nil, wrapError(CodeInviteInvalid, "encode invite", err)
	}
	if len(raw) > maxInviteBytes {
		return nil, newError(CodeInviteInvalid, "invite exceeds size limit")
	}
	return raw, nil
}

// ParseInvite decodes and validates one invite. Unknown fields and trailing
// data are rejected. Error text never contains the invite payload.
func ParseInvite(raw []byte, now time.Time) (Invite, error) {
	if len(raw) == 0 || len(raw) > maxInviteBytes {
		return Invite{}, newError(CodeInviteInvalid, "invite size out of range")
	}
	var invite Invite
	if err := decodeStrict(raw, &invite); err != nil {
		return Invite{}, newError(CodeInviteInvalid, "invite is not valid JSON")
	}
	if err := invite.Validate(now); err != nil {
		return Invite{}, err
	}
	return invite, nil
}

// Validate checks version, identifiers, keys, expiry and relay origin.
func (i Invite) Validate(now time.Time) error {
	if i.V != 1 {
		return newError(CodeInviteInvalid, "unsupported invite version")
	}
	if !validIdentifier(i.HostID) {
		return newError(CodeInviteInvalid, "invalid host id")
	}
	if !validIdentifier(i.InviteID) {
		return newError(CodeInviteInvalid, "invalid invite id")
	}
	if i.HostEd.IsZero() || i.HostX.IsZero() {
		return newError(CodeInviteInvalid, "host keys required")
	}
	var zeroSecret Secret
	if i.Secret == zeroSecret {
		return newError(CodeInviteInvalid, "invite secret required")
	}
	if err := validateRelay(i.Relay); err != nil {
		return err
	}
	nowMS := now.UnixMilli()
	if i.ExpireMS <= nowMS {
		return newError(CodeInviteExpired, "invite expired")
	}
	if i.ExpireMS-nowMS > MaxInviteTTL.Milliseconds() {
		return newError(CodeInviteInvalid, "invite expiry exceeds five minutes")
	}
	return nil
}

func validateRelay(raw string) error {
	if raw == "" {
		return newError(CodeInviteInvalid, "relay required")
	}
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "wss" || u.Host == "" || u.User != nil {
		return newError(CodeInviteInvalid, "relay must be an absolute wss URL")
	}
	if u.Port() == "0" {
		return newError(CodeInviteInvalid, "relay port invalid")
	}
	return nil
}

func validIdentifier(s string) bool {
	if len(s) == 0 || len(s) > 128 {
		return false
	}
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9':
		case c == '-', c == '_', c == '.', c == ':':
		default:
			return false
		}
	}
	return true
}

type inviteEntry struct {
	invite     Invite
	inProgress bool
	used       bool
	usedAt     time.Time
}

// InviteStore tracks single-use invites. Begin atomically claims an invite
// before any handshake work; Complete or Burn makes the claim permanent.
type InviteStore struct {
	mu      sync.Mutex
	entries map[string]*inviteEntry
	now     func() time.Time
}

// NewInviteStore returns an empty store.
func NewInviteStore() *InviteStore {
	return &InviteStore{entries: make(map[string]*inviteEntry), now: time.Now}
}

func (s *InviteStore) clock() time.Time {
	if s.now == nil {
		return time.Now()
	}
	return s.now()
}

// Add registers an invite. The invite must be valid at the current time and
// its id must be unused.
func (s *InviteStore) Add(invite Invite) error {
	return s.addAt(invite, s.clock())
}

func (s *InviteStore) addAt(invite Invite, now time.Time) error {
	if err := invite.Validate(now); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.entries == nil {
		s.entries = make(map[string]*inviteEntry)
	}
	if _, exists := s.entries[invite.InviteID]; exists {
		return newError(CodeInviteInvalid, "duplicate invite id")
	}
	s.entries[invite.InviteID] = &inviteEntry{invite: invite}
	return nil
}

// Begin atomically marks an invite in progress and returns a copy. The caller
// owns the returned secret copy and should zeroize it after use.
func (s *InviteStore) Begin(inviteID string) (Invite, error) {
	return s.beginAt(inviteID, s.clock())
}

func (s *InviteStore) beginAt(inviteID string, now time.Time) (Invite, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	entry, ok := s.entries[inviteID]
	if !ok {
		return Invite{}, newError(CodeInviteUnknown, "unknown invite")
	}
	if entry.used {
		return Invite{}, newError(CodeInviteUsed, "invite already used")
	}
	if entry.inProgress {
		return Invite{}, newError(CodeInviteInProgress, "invite already in progress")
	}
	if entry.invite.ExpireMS <= now.UnixMilli() {
		entry.used = true
		entry.usedAt = now
		entry.invite.Secret.Zeroize()
		return Invite{}, newError(CodeInviteExpired, "invite expired")
	}
	entry.inProgress = true
	return entry.invite, nil
}

// Complete records a successful pairing. The stored secret is erased.
func (s *InviteStore) Complete(inviteID string) error {
	return s.finish(inviteID)
}

// Burn records a failed pairing. A failed handshake permanently spends the
// invite.
func (s *InviteStore) Burn(inviteID string) error {
	return s.finish(inviteID)
}

func (s *InviteStore) finish(inviteID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	entry, ok := s.entries[inviteID]
	if !ok {
		return newError(CodeInviteUnknown, "unknown invite")
	}
	entry.inProgress = false
	entry.used = true
	entry.usedAt = s.clock()
	entry.invite.Secret.Zeroize()
	return nil
}

// Reap removes used invites older than one hour. Unused, unexpired invites are
// never removed.
func (s *InviteStore) Reap(now time.Time) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, entry := range s.entries {
		if entry.used && now.Sub(entry.usedAt) > time.Hour {
			delete(s.entries, id)
		}
	}
}
