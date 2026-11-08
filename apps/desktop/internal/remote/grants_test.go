package remote

import (
	"testing"
	"time"
)

func mustKeys(t *testing.T) (Key, Key) {
	t.Helper()
	ed, err := randomKey()
	if err != nil {
		t.Fatal(err)
	}
	x, err := randomKey()
	if err != nil {
		t.Fatal(err)
	}
	return ed, x
}

func mustRegistryDevice(t *testing.T, registry *Registry, id string, now time.Time) Device {
	t.Helper()
	ed, x := mustKeys(t)
	device := Device{ID: id, Ed: ed, X: x}
	if err := registry.Register(device, now); err != nil {
		t.Fatalf("register: %v", err)
	}
	registered, ok := registry.Device(id)
	if !ok {
		t.Fatal("registered device not found")
	}
	return registered
}

func TestRegistryGrantAndAuthorize(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)

	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeWorkspaceRead, Subject: SubjectHost,
		ExpiresAt: now.Add(time.Hour),
	}, now); err != nil {
		t.Fatalf("grant: %v", err)
	}
	if err := registry.Authorize(device.ID, ScopeWorkspaceRead, SubjectSession("s1"), device.Generation, now); err != nil {
		t.Fatalf("host grant should cover session subject: %v", err)
	}
	if err := registry.Authorize(device.ID, ScopeSessionObserve, SubjectSession("s1"), device.Generation, now); !HasCode(err, CodeMissingScope) {
		t.Fatalf("missing scope error = %v", err)
	}
	if err := registry.Authorize(device.ID, ScopeWorkspaceRead, "invalid-subject", device.Generation, now); !HasCode(err, CodeSubjectInvalid) {
		t.Fatalf("subject error = %v", err)
	}
	if err := registry.Authorize(device.ID, Scope("workspace.write"), SubjectHost, device.Generation, now); !HasCode(err, CodeScopeUnknown) {
		t.Fatalf("scope error = %v", err)
	}
	if err := registry.Authorize("missing", ScopeWorkspaceRead, SubjectHost, 1, now); !HasCode(err, CodeUnknownDevice) {
		t.Fatalf("unknown device error = %v", err)
	}
	if err := registry.Authorize(device.ID, ScopeWorkspaceRead, SubjectHost, device.Generation+1, now); !HasCode(err, CodeStaleIdentity) {
		t.Fatalf("generation error = %v", err)
	}
}

func TestRegistrySensitiveTTLAndDeviceManage(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)

	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeScreenView, Subject: SubjectSession("s1"),
		ExpiresAt: now.Add(2 * time.Hour),
	}, now); !HasCode(err, CodeGrantInvalid) {
		t.Fatalf("long sensitive TTL error = %v", err)
	}
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeScreenView, Subject: SubjectSession("s1"),
		ExpiresAt: now.Add(30 * time.Minute),
	}, now); err != nil {
		t.Fatalf("sensitive grant: %v", err)
	}
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeAgentApprove, Subject: SubjectHost,
		ExpiresAt: now.Add(MaxSensitiveTTL + time.Second),
	}, now); !HasCode(err, CodeGrantInvalid) {
		t.Fatalf("approve TTL error = %v", err)
	}
	if err := registry.GrantFromPairing(Grant{
		DeviceID: device.ID, Scope: ScopeDeviceManage, Subject: SubjectHost,
		ExpiresAt: now.Add(time.Hour),
	}, now); !HasCode(err, CodeGrantNotGrantable) {
		t.Fatalf("pairing device.manage error = %v", err)
	}
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeDeviceManage, Subject: SubjectHost,
		ExpiresAt: now.Add(time.Hour),
	}, now); err != nil {
		t.Fatalf("operator device.manage grant: %v", err)
	}
}

func TestRegistryAuthorizeRejectsRestoredLongSensitiveGrant(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)
	// Seed the store directly to model a persisted grant that bypassed
	// Grant's issuance checks.
	registry.mu.Lock()
	registry.devices[device.ID].grants[grantKey{scope: ScopeScreenControl, subject: SubjectHost}] = Grant{
		DeviceID: device.ID, Scope: ScopeScreenControl, Subject: SubjectHost,
		ExpiresAt: now.Add(2 * time.Hour),
	}
	registry.mu.Unlock()
	if err := registry.Authorize(device.ID, ScopeScreenControl, SubjectHost, device.Generation, now); !HasCode(err, CodeGrantInvalid) {
		t.Fatalf("error = %v, want grant_invalid", err)
	}
}

func TestRegistryGrantExpiryIsLive(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeSessionObserve, Subject: SubjectSession("s1"),
		ExpiresAt: now.Add(50 * time.Millisecond),
	}, now); err != nil {
		t.Fatal(err)
	}
	if err := registry.Authorize(device.ID, ScopeSessionObserve, SubjectSession("s1"), device.Generation, now.Add(100*time.Millisecond)); !HasCode(err, CodeGrantExpired) {
		t.Fatalf("expired error = %v", err)
	}
	if grants := registry.Grants(device.ID); len(grants) != 0 {
		t.Fatalf("expired grant still listed: %+v", grants)
	}
	if err := registry.Authorize(device.ID, ScopeSessionObserve, SubjectSession("s1"), device.Generation, now); !HasCode(err, CodeMissingScope) {
		t.Fatalf("after expiry error = %v", err)
	}
}

func TestRegistryRevokeClearsAndBumpsGeneration(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeAgentSend, Subject: SubjectSession("s1"),
		ExpiresAt: now.Add(time.Hour),
	}, now); err != nil {
		t.Fatal(err)
	}
	if err := registry.Revoke(device.ID, now); err != nil {
		t.Fatalf("revoke: %v", err)
	}
	revoked, _ := registry.Device(device.ID)
	if !revoked.Revoked {
		t.Fatal("device not marked revoked")
	}
	if revoked.Generation != device.Generation+1 {
		t.Fatalf("generation = %d, want %d", revoked.Generation, device.Generation+1)
	}
	if grants := registry.Grants(device.ID); len(grants) != 0 {
		t.Fatalf("grants not cleared: %+v", grants)
	}
	if err := registry.Authorize(device.ID, ScopeAgentSend, SubjectSession("s1"), revoked.Generation, now); !HasCode(err, CodeDeviceRevoked) {
		t.Fatalf("authorize after revoke error = %v", err)
	}
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeAgentSend, Subject: SubjectHost,
		ExpiresAt: now.Add(time.Hour),
	}, now); !HasCode(err, CodeDeviceRevoked) {
		t.Fatalf("grant after revoke error = %v", err)
	}
	if err := registry.Revoke(device.ID, now); err != nil {
		t.Fatalf("second revoke: %v", err)
	}
	if !registry.Revoked(device.ID) {
		t.Fatal("Revoked() = false")
	}
}

func TestRegistryRotateInvalidatesOldGrants(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeGitWrite, Subject: SubjectHost,
		ExpiresAt: now.Add(time.Hour),
	}, now); err != nil {
		t.Fatal(err)
	}
	ed, x := mustKeys(t)
	if err := registry.Rotate(device.ID, ed, x, now); err != nil {
		t.Fatalf("rotate: %v", err)
	}
	rotated, _ := registry.Device(device.ID)
	if rotated.Generation != device.Generation+1 {
		t.Fatalf("generation = %d", rotated.Generation)
	}
	if err := registry.Authorize(device.ID, ScopeGitWrite, SubjectHost, device.Generation, now); !HasCode(err, CodeStaleIdentity) {
		t.Fatalf("old generation error = %v", err)
	}
	if err := registry.Authorize(device.ID, ScopeGitWrite, SubjectHost, rotated.Generation, now); !HasCode(err, CodeMissingScope) {
		t.Fatalf("rotated grant error = %v", err)
	}
}

func TestRegistryRejectsDuplicateAndInvalid(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	device := mustRegistryDevice(t, registry, "device-1", now)
	if err := registry.Register(device, now); !HasCode(err, CodeDeviceExists) {
		t.Fatalf("duplicate error = %v", err)
	}
	if err := registry.Register(Device{ID: "", Ed: device.Ed, X: device.X}, now); !HasCode(err, CodeGrantInvalid) {
		t.Fatalf("invalid id error = %v", err)
	}
	if err := registry.Register(Device{ID: "device-2"}, now); !HasCode(err, CodeGrantInvalid) {
		t.Fatalf("missing keys error = %v", err)
	}
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeWorkspaceRead, Subject: "space:",
		ExpiresAt: now.Add(time.Hour),
	}, now); !HasCode(err, CodeSubjectInvalid) {
		t.Fatalf("empty space error = %v", err)
	}
	if err := registry.Grant(Grant{
		DeviceID: device.ID, Scope: ScopeWorkspaceRead, Subject: SubjectHost,
		ExpiresAt: now,
	}, now); !HasCode(err, CodeGrantInvalid) {
		t.Fatalf("expired issue error = %v", err)
	}
}
