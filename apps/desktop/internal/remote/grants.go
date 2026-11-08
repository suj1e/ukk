package remote

import (
	"sort"
	"strings"
	"sync"
	"time"
)

// Scope is a device capability. Scopes are additive; holding one never implies
// another.
type Scope string

const (
	ScopeWorkspaceRead  Scope = "workspace.read"
	ScopeSessionObserve Scope = "session.observe"
	ScopeAgentSend      Scope = "agent.send"
	ScopeAgentApprove   Scope = "agent.approve"
	ScopeFilesystemRead Scope = "filesystem.read"
	ScopeGitWrite       Scope = "git.write"
	ScopeScreenView     Scope = "screen.view"
	ScopeScreenControl  Scope = "screen.control"
	ScopeScreenAudio    Scope = "screen.audio"
	ScopePluginControl  Scope = "plugin.control"
	ScopeDeviceManage   Scope = "device.manage"
)

// MaxSensitiveTTL bounds sensitive-scope grants.
const MaxSensitiveTTL = time.Hour

var allScopes = map[Scope]bool{
	ScopeWorkspaceRead:  true,
	ScopeSessionObserve: true,
	ScopeAgentSend:      true,
	ScopeAgentApprove:   true,
	ScopeFilesystemRead: true,
	ScopeGitWrite:       true,
	ScopeScreenView:     true,
	ScopeScreenControl:  true,
	ScopeScreenAudio:    true,
	ScopePluginControl:  true,
	ScopeDeviceManage:   true,
}

// Valid reports whether the scope is known. Unknown scopes fail closed.
func (s Scope) Valid() bool { return allScopes[s] }

// Sensitive reports whether the scope has a short TTL limit.
func (s Scope) Sensitive() bool {
	switch s {
	case ScopeAgentApprove, ScopeScreenView, ScopeScreenControl, ScopeScreenAudio, ScopePluginControl:
		return true
	default:
		return false
	}
}

// SubjectHost is the wildcard subject covering every scope subject.
const SubjectHost = "host"

// SubjectSpace returns a space-scoped subject.
func SubjectSpace(id string) string { return "space:" + id }

// SubjectSession returns a session-scoped subject.
func SubjectSession(id string) string { return "session:" + id }

func validSubject(subject string) bool {
	if subject == SubjectHost {
		return true
	}
	if rest, ok := strings.CutPrefix(subject, "space:"); ok {
		return validIdentifier(rest)
	}
	if rest, ok := strings.CutPrefix(subject, "session:"); ok {
		return validIdentifier(rest)
	}
	return false
}

// Grant authorizes one device, one scope and one subject until ExpiresAt.
type Grant struct {
	DeviceID  string
	Scope     Scope
	Subject   string
	ExpiresAt time.Time
}

// Device is a registered device identity. Generation changes on revocation or
// key rotation; sessions built with an older generation fail authorization.
type Device struct {
	ID         string
	Ed         Key
	X          Key
	Generation uint64
	Revoked    bool
	RevokedAt  time.Time
	CreatedAt  time.Time
}

type grantKey struct {
	scope   Scope
	subject string
}

type deviceState struct {
	device Device
	grants map[grantKey]Grant
}

// Registry is the live device and grant store. Every authorization re-reads
// revocation, generation, grant presence and expiry; cached handshake scopes
// are never authoritative.
type Registry struct {
	mu      sync.Mutex
	devices map[string]*deviceState
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{devices: make(map[string]*deviceState)}
}

// Register adds a device. The generation defaults to 1.
func (r *Registry) Register(device Device, now time.Time) error {
	if !validIdentifier(device.ID) {
		return newError(CodeGrantInvalid, "invalid device id")
	}
	if device.Ed.IsZero() || device.X.IsZero() {
		return newError(CodeGrantInvalid, "device keys required")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, exists := r.devices[device.ID]; exists {
		return newError(CodeDeviceExists, "device already registered")
	}
	if device.Generation == 0 {
		device.Generation = 1
	}
	if device.CreatedAt.IsZero() {
		device.CreatedAt = now
	}
	device.Revoked = false
	device.RevokedAt = time.Time{}
	r.devices[device.ID] = &deviceState{device: device, grants: make(map[grantKey]Grant)}
	return nil
}

// Grant issues or replaces one grant.
func (r *Registry) Grant(grant Grant, now time.Time) error {
	return r.grant(grant, now, true)
}

// GrantFromPairing issues a grant from a pairing flow. device.manage can never
// be granted this way.
func (r *Registry) GrantFromPairing(grant Grant, now time.Time) error {
	if grant.Scope == ScopeDeviceManage {
		return newError(CodeGrantNotGrantable, "device.manage cannot be granted by pairing")
	}
	return r.grant(grant, now, false)
}

func (r *Registry) grant(grant Grant, now time.Time, allowDeviceManage bool) error {
	if !grant.Scope.Valid() {
		return newError(CodeScopeUnknown, "unknown scope")
	}
	if !allowDeviceManage && grant.Scope == ScopeDeviceManage {
		return newError(CodeGrantNotGrantable, "device.manage cannot be granted by pairing")
	}
	if !validSubject(grant.Subject) {
		return newError(CodeSubjectInvalid, "invalid grant subject")
	}
	if !grant.ExpiresAt.After(now) {
		return newError(CodeGrantInvalid, "grant expiry must be in the future")
	}
	if grant.Scope.Sensitive() && grant.ExpiresAt.Sub(now) > MaxSensitiveTTL {
		return newError(CodeGrantInvalid, "sensitive scope TTL exceeds one hour")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[grant.DeviceID]
	if !ok {
		return newError(CodeUnknownDevice, "unknown device")
	}
	if state.device.Revoked {
		return newError(CodeDeviceRevoked, "device revoked")
	}
	state.grants[grantKey{scope: grant.Scope, subject: grant.Subject}] = grant
	return nil
}

// Revoke marks the device revoked, bumps its generation and clears every
// grant. Revocation is idempotent.
func (r *Registry) Revoke(deviceID string, now time.Time) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[deviceID]
	if !ok {
		return newError(CodeUnknownDevice, "unknown device")
	}
	if state.device.Revoked {
		state.grants = make(map[grantKey]Grant)
		return nil
	}
	state.device.Revoked = true
	state.device.RevokedAt = now
	state.device.Generation++
	state.grants = make(map[grantKey]Grant)
	return nil
}

// Rotate replaces the device keys, bumps the generation and clears grants.
func (r *Registry) Rotate(deviceID string, ed, x Key, now time.Time) error {
	if ed.IsZero() || x.IsZero() {
		return newError(CodeGrantInvalid, "device keys required")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[deviceID]
	if !ok {
		return newError(CodeUnknownDevice, "unknown device")
	}
	if state.device.Revoked {
		return newError(CodeDeviceRevoked, "device revoked")
	}
	state.device.Ed = ed
	state.device.X = x
	state.device.Generation++
	state.grants = make(map[grantKey]Grant)
	return nil
}

// Authorize performs a live check for one operation. generation must be the
// generation observed by the authenticated session. A host-subject grant
// covers every subject for its scope.
func (r *Registry) Authorize(deviceID string, scope Scope, subject string, generation uint64, now time.Time) error {
	if !scope.Valid() {
		return newError(CodeScopeUnknown, "unknown scope")
	}
	if !validSubject(subject) {
		return newError(CodeSubjectInvalid, "invalid subject")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[deviceID]
	if !ok {
		return newError(CodeUnknownDevice, "unknown device")
	}
	if state.device.Revoked {
		return newError(CodeDeviceRevoked, "device revoked")
	}
	if generation == 0 || generation != state.device.Generation {
		return newError(CodeStaleIdentity, "stale device generation")
	}
	grant, ok := state.grants[grantKey{scope: scope, subject: subject}]
	if !ok {
		grant, ok = state.grants[grantKey{scope: scope, subject: SubjectHost}]
	}
	if !ok {
		return newError(CodeMissingScope, "missing scope")
	}
	if !now.Before(grant.ExpiresAt) {
		delete(state.grants, grantKey{scope: grant.Scope, subject: grant.Subject})
		return newError(CodeGrantExpired, "grant expired")
	}
	// Sensitive grants must never outlive one hour, even when they were
	// restored from persistence without going through Grant.
	if scope.Sensitive() && grant.ExpiresAt.Sub(now) > MaxSensitiveTTL {
		return newError(CodeGrantInvalid, "sensitive grant exceeds one hour")
	}
	return nil
}

// Device returns a copy of a registered device.
func (r *Registry) Device(deviceID string) (Device, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[deviceID]
	if !ok {
		return Device{}, false
	}
	return state.device, true
}

// Grants returns a device's grants ordered by scope and subject.
func (r *Registry) Grants(deviceID string) []Grant {
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[deviceID]
	if !ok {
		return nil
	}
	out := make([]Grant, 0, len(state.grants))
	for _, grant := range state.grants {
		out = append(out, grant)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Scope == out[j].Scope {
			return out[i].Subject < out[j].Subject
		}
		return out[i].Scope < out[j].Scope
	})
	return out
}

// Revoked reports whether the device is revoked.
func (r *Registry) Revoked(deviceID string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	state, ok := r.devices[deviceID]
	return !ok || state.device.Revoked
}
