package remotescreen

import (
	"errors"
	"fmt"
	"sync"
)

// Authorization actions. Viewing is required before capture starts; every
// injected input event requires control. Pairing and Agent control never
// imply either action.
const (
	ActionScreenView    = "screen.view"
	ActionScreenControl = "screen.control"
)

// Authorizer decides whether an action is currently allowed. Implementations
// must combine the local device grant with any OS permission check; a grant
// alone is never enough for screen.control.
type Authorizer interface {
	Allow(action string) error
}

var (
	// ErrNotAuthorized reports a missing grant, an expired lease, or a
	// denied OS permission.
	ErrNotAuthorized = errors.New("remotescreen: action not authorized")
	// ErrUnknownAction reports an action outside screen.view/screen.control.
	ErrUnknownAction = errors.New("remotescreen: unknown authorization action")
)

// GrantAuthorizer is the in-process authorizer used by a live Session. Grants
// are explicit and revocable; control also consults an optional OS
// permission probe.
type GrantAuthorizer struct {
	mu     sync.RWMutex
	grants map[string]bool
	checks map[string]func() error
}

// NewGrantAuthorizer returns a deny-all authorizer.
func NewGrantAuthorizer() *GrantAuthorizer {
	return &GrantAuthorizer{
		grants: make(map[string]bool),
		checks: make(map[string]func() error),
	}
}

// Grant allows action until Revoke is called. Unknown actions are rejected.
func (a *GrantAuthorizer) Grant(action string) error {
	if err := validateAction(action); err != nil {
		return err
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	a.grants[action] = true
	return nil
}

// Revoke withdraws action immediately. In-flight capture or input calls
// observe the revocation on their next authorization check.
func (a *GrantAuthorizer) Revoke(action string) error {
	if err := validateAction(action); err != nil {
		return err
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	delete(a.grants, action)
	return nil
}

// RevokeAll withdraws every grant.
func (a *GrantAuthorizer) RevokeAll() {
	a.mu.Lock()
	defer a.mu.Unlock()
	clear(a.grants)
}

// SetOSPermission installs an OS-level permission probe for action. A nil
// probe removes it. The probe is called on every Allow, so permission
// revocation takes effect without restarting capture.
func (a *GrantAuthorizer) SetOSPermission(action string, check func() error) error {
	if err := validateAction(action); err != nil {
		return err
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	if check == nil {
		delete(a.checks, action)
	} else {
		a.checks[action] = check
	}
	return nil
}

// Allow implements Authorizer.
func (a *GrantAuthorizer) Allow(action string) error {
	if err := validateAction(action); err != nil {
		return err
	}
	a.mu.RLock()
	granted := a.grants[action]
	check := a.checks[action]
	a.mu.RUnlock()

	if !granted {
		return fmt.Errorf("%w: %s grant missing", ErrNotAuthorized, action)
	}
	if check != nil {
		if err := check(); err != nil {
			return fmt.Errorf("%w: %s OS permission: %v", ErrNotAuthorized, action, err)
		}
	}
	return nil
}

// Granted reports whether action currently has an explicit grant. It does not
// evaluate OS permissions.
func (a *GrantAuthorizer) Granted(action string) bool {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.grants[action]
}

func validateAction(action string) error {
	switch action {
	case ActionScreenView, ActionScreenControl:
		return nil
	default:
		return fmt.Errorf("%w: %q", ErrUnknownAction, action)
	}
}
