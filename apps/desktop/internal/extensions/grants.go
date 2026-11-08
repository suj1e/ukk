package extensions

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"
)

// Grant scopes. A host grant covers every scope; a session grant covers only
// that session. Space scopes are stored but do not inherit down to sessions in
// v1, because there is no session-to-space mapping here to guess from.
const (
	ScopeHost          = "host"
	ScopeSpacePrefix   = "space:"
	ScopeSessionPrefix = "session:"
)

// Grant TTL ceilings.
const (
	MaxGrantTTL       = 24 * time.Hour
	MaxSensitiveTTL   = time.Hour
	MaxGrantFileBytes = 1 << 20
)

// ValidateScope reports whether scope is host, space:<id> or session:<id>.
func ValidateScope(scope string) error {
	if scope == ScopeHost {
		return nil
	}
	if strings.HasPrefix(scope, ScopeSpacePrefix) {
		return validateScopeID(scope[len(ScopeSpacePrefix):], ScopeSpacePrefix)
	}
	if strings.HasPrefix(scope, ScopeSessionPrefix) {
		return validateScopeID(scope[len(ScopeSessionPrefix):], ScopeSessionPrefix)
	}
	return errf(CodeInvalidScope, "scope must be host, space:<id> or session:<id>")
}

func validateScopeID(id, prefix string) error {
	if id == "" || len(id) > MaxIDLength || !contributionPattern.MatchString(id) || strings.Contains(id, "..") {
		return errf(CodeInvalidScope, "%s scope has an invalid id", strings.TrimSuffix(prefix, ":"))
	}
	return nil
}

// SessionScope builds a session-scoped grant scope.
func SessionScope(sessionID string) string { return ScopeSessionPrefix + sessionID }

// SpaceScope builds a space-scoped grant scope.
func SpaceScope(spaceID string) string { return ScopeSpacePrefix + spaceID }

// Grant is one capability grant for one plugin and scope. An empty
// ExpiresAtMS means no expiry; sensitive grants are never allowed to omit it.
type Grant struct {
	Plugin      string `json:"plugin"`
	Capability  string `json:"capability"`
	Scope       string `json:"scope"`
	GrantedAtMS int64  `json:"granted_at_ms"`
	ExpiresAtMS int64  `json:"expires_at_ms,omitempty"`
}

// Active reports whether the grant is still live at now. Expiry is checked at
// use time, not cached, and the boundary is exclusive.
func (g Grant) Active(now time.Time) bool {
	if g.ExpiresAtMS == 0 {
		return true
	}
	return now.UnixMilli() < g.ExpiresAtMS
}

type grantKey struct {
	plugin, capability, scope string
}

type grantDocument struct {
	Version int     `json:"version"`
	Grants  []Grant `json:"grants"`
}

// GrantStore is a concurrency-safe grant table with optional JSON persistence.
// Installing or enabling a plugin creates no grants; there is no default
// allow. Revocation deletes the persisted row, so reopening the store does not
// resurrect it.
type GrantStore struct {
	mu     sync.Mutex
	path   string
	grants map[grantKey]Grant
}

// OpenGrantStore opens or creates a grant store. A non-empty path is loaded
// strictly and must be a regular 0600 file; an empty path is in-memory only.
func OpenGrantStore(path string) (*GrantStore, error) {
	store := &GrantStore{path: path, grants: make(map[grantKey]Grant)}
	if path == "" {
		return store, nil
	}
	if err := store.load(); err != nil {
		return nil, err
	}
	return store, nil
}

func (s *GrantStore) load() error {
	info, err := os.Stat(s.path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return errf(CodeInvalidGrant, "stat grant store: %s", err.Error())
	}
	if !info.Mode().IsRegular() {
		return errf(CodeInvalidGrant, "grant store %s is not a regular file", s.path)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		return errf(CodeInsecurePermission, "grant store %s must be 0600", s.path)
	}
	data, err := readGrantFile(s.path)
	if err != nil {
		return err
	}
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	var doc grantDocument
	if err := dec.Decode(&doc); err != nil {
		return errf(CodeInvalidGrant, "decode grant store: %s", err.Error())
	}
	var extra json.RawMessage
	if err := dec.Decode(&extra); err != io.EOF {
		return errf(CodeInvalidGrant, "trailing data in grant store")
	}
	if doc.Version != 1 {
		return errf(CodeInvalidGrant, "unsupported grant store version %d", doc.Version)
	}
	loaded := make(map[grantKey]Grant, len(doc.Grants))
	for _, grant := range doc.Grants {
		if err := validateGrantRecord(grant); err != nil {
			return err
		}
		key := grantKey{plugin: grant.Plugin, capability: grant.Capability, scope: grant.Scope}
		if _, duplicate := loaded[key]; duplicate {
			return errf(CodeInvalidGrant, "duplicate grant for %s/%s/%s", grant.Plugin, grant.Capability, grant.Scope)
		}
		loaded[key] = grant
	}
	s.grants = loaded
	return nil
}

func readGrantFile(path string) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, errf(CodeInvalidGrant, "read grant store: %s", err.Error())
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, MaxGrantFileBytes+1))
	if err != nil {
		return nil, errf(CodeInvalidGrant, "read grant store: %s", err.Error())
	}
	if len(data) > MaxGrantFileBytes {
		return nil, errf(CodeLimitExceeded, "grant store exceeds %d bytes", MaxGrantFileBytes)
	}
	return data, nil
}

func validateGrantRecord(grant Grant) error {
	if err := validatePluginID(grant.Plugin); err != nil {
		return err
	}
	if _, err := CapabilityRisk(grant.Capability); err != nil {
		return err
	}
	if err := ValidateScope(grant.Scope); err != nil {
		return err
	}
	if grant.GrantedAtMS < 0 || grant.ExpiresAtMS < 0 {
		return errf(CodeInvalidGrant, "grant timestamps must not be negative")
	}
	return nil
}

// Grant calls GrantAt with the wall clock.
func (s *GrantStore) Grant(plugin, capability, scope string, ttl time.Duration) (Grant, error) {
	return s.GrantAt(plugin, capability, scope, ttl, time.Now())
}

// GrantAt issues or replaces a grant at the supplied instant. Risk class rules
// are enforced here rather than in the UI: sensitive capabilities require a
// TTL of at most one hour, mutate capabilities allow at most 24 hours, and
// observe capabilities may be long-lived.
func (s *GrantStore) GrantAt(plugin, capability, scope string, ttl time.Duration, now time.Time) (Grant, error) {
	if err := validatePluginID(plugin); err != nil {
		return Grant{}, err
	}
	risk, err := CapabilityRisk(capability)
	if err != nil {
		return Grant{}, err
	}
	if err := ValidateScope(scope); err != nil {
		return Grant{}, err
	}
	if ttl < 0 {
		return Grant{}, errf(CodeInvalidGrant, "ttl must not be negative")
	}
	switch risk {
	case RiskSensitive:
		if ttl == 0 {
			return Grant{}, errf(CodeInvalidGrant, "capability %s requires a TTL of at most %s", capability, MaxSensitiveTTL)
		}
		if ttl > MaxSensitiveTTL {
			return Grant{}, errf(CodeInvalidGrant, "capability %s TTL must not exceed %s", capability, MaxSensitiveTTL)
		}
	case RiskMutate:
		if ttl > MaxGrantTTL {
			return Grant{}, errf(CodeInvalidGrant, "capability %s TTL must not exceed %s", capability, MaxGrantTTL)
		}
	}

	grant := Grant{
		Plugin:      plugin,
		Capability:  capability,
		Scope:       scope,
		GrantedAtMS: now.UnixMilli(),
	}
	if ttl > 0 {
		grant.ExpiresAtMS = now.Add(ttl).UnixMilli()
	}

	key := grantKey{plugin: plugin, capability: capability, scope: scope}
	s.mu.Lock()
	defer s.mu.Unlock()
	previous, had := s.grants[key]
	s.grants[key] = grant
	if err := s.saveLocked(); err != nil {
		if had {
			s.grants[key] = previous
		} else {
			delete(s.grants, key)
		}
		return Grant{}, err
	}
	return grant, nil
}

// Revoke removes one grant and persists the removal. A revoked grant does not
// come back when the store is reopened.
func (s *GrantStore) Revoke(plugin, capability, scope string) (bool, error) {
	if err := validatePluginID(plugin); err != nil {
		return false, err
	}
	if _, err := CapabilityRisk(capability); err != nil {
		return false, err
	}
	if err := ValidateScope(scope); err != nil {
		return false, err
	}
	key := grantKey{plugin: plugin, capability: capability, scope: scope}
	s.mu.Lock()
	defer s.mu.Unlock()
	previous, ok := s.grants[key]
	if !ok {
		return false, nil
	}
	delete(s.grants, key)
	if err := s.saveLocked(); err != nil {
		s.grants[key] = previous
		return false, err
	}
	return true, nil
}

// RevokeAll removes every grant for a plugin and returns the count.
func (s *GrantStore) RevokeAll(plugin string) (int, error) {
	if err := validatePluginID(plugin); err != nil {
		return 0, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	removed := make(map[grantKey]Grant)
	for key, grant := range s.grants {
		if key.plugin == plugin {
			removed[key] = grant
			delete(s.grants, key)
		}
	}
	if len(removed) == 0 {
		return 0, nil
	}
	if err := s.saveLocked(); err != nil {
		for key, grant := range removed {
			s.grants[key] = grant
		}
		return 0, err
	}
	return len(removed), nil
}

// Authorized checks the live grant table for a capability in a scope. A host
// grant covers every scope; any other grant must match the requested scope
// exactly.
func (s *GrantStore) Authorized(plugin, capability, scope string, now time.Time) bool {
	if ValidateScope(scope) != nil {
		return false
	}
	if _, err := CapabilityRisk(capability); err != nil {
		return false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, grant := range s.grants {
		if grant.Plugin != plugin || grant.Capability != capability {
			continue
		}
		if !grant.Active(now) {
			continue
		}
		if grant.Scope == ScopeHost || grant.Scope == scope {
			return true
		}
	}
	return false
}

// Grants returns a sorted copy of the stored grants.
func (s *GrantStore) Grants() []Grant {
	s.mu.Lock()
	defer s.mu.Unlock()
	return sortedGrants(s.grants)
}

// Len returns the number of stored grant rows.
func (s *GrantStore) Len() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.grants)
}

func sortedGrants(grants map[grantKey]Grant) []Grant {
	out := make([]Grant, 0, len(grants))
	for _, grant := range grants {
		out = append(out, grant)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Plugin != out[j].Plugin {
			return out[i].Plugin < out[j].Plugin
		}
		if out[i].Capability != out[j].Capability {
			return out[i].Capability < out[j].Capability
		}
		return out[i].Scope < out[j].Scope
	})
	return out
}

func (s *GrantStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	doc := grantDocument{Version: 1, Grants: sortedGrants(s.grants)}
	data, err := json.Marshal(doc)
	if err != nil {
		return errf(CodeInvalidGrant, "encode grant store: %s", err.Error())
	}
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return errf(CodeInvalidGrant, "create grant directory: %s", err.Error())
	}
	tmp, err := os.CreateTemp(dir, ".grants-*.tmp")
	if err != nil {
		return errf(CodeInvalidGrant, "create grant store: %s", err.Error())
	}
	name := tmp.Name()
	cleanup := func() {
		tmp.Close()
		os.Remove(name)
	}
	if err := tmp.Chmod(0o600); err != nil {
		cleanup()
		return errf(CodeInsecurePermission, "chmod grant store: %s", err.Error())
	}
	if _, err := tmp.Write(data); err != nil {
		cleanup()
		return errf(CodeInvalidGrant, "write grant store: %s", err.Error())
	}
	if err := tmp.Sync(); err != nil {
		cleanup()
		return errf(CodeInvalidGrant, "sync grant store: %s", err.Error())
	}
	if err := tmp.Close(); err != nil {
		os.Remove(name)
		return errf(CodeInvalidGrant, "close grant store: %s", err.Error())
	}
	if err := os.Rename(name, s.path); err != nil {
		os.Remove(name)
		return errf(CodeInvalidGrant, "replace grant store: %s", err.Error())
	}
	return nil
}
