package remote

import (
	"testing"
	"time"
)

func TestRegistrySnapshotRestore(t *testing.T) {
	now := time.Now()
	registry := NewRegistry()
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatal(err)
	}
	pub := id.Public()
	device := Device{ID: "dev_test", Ed: pub.Ed, X: pub.X, CreatedAt: now}
	if err := registry.Register(device, now); err != nil {
		t.Fatal(err)
	}
	grant := Grant{DeviceID: device.ID, Scope: ScopeSessionObserve, Subject: SubjectHost, ExpiresAt: now.Add(time.Hour)}
	if err := registry.Grant(grant, now); err != nil {
		t.Fatal(err)
	}
	devices, grants := registry.Snapshot()
	if len(devices) != 1 || len(grants) != 1 {
		t.Fatalf("snapshot = %d devices, %d grants", len(devices), len(grants))
	}
	other := NewRegistry()
	if err := other.Restore(devices, grants, now); err != nil {
		t.Fatal(err)
	}
	if err := other.Authorize(device.ID, ScopeSessionObserve, SubjectHost, devices[0].Generation, now); err != nil {
		t.Fatalf("authorize restored grant: %v", err)
	}
	if err := other.Authorize(device.ID, ScopeAgentApprove, SubjectHost, devices[0].Generation, now); err == nil {
		t.Fatal("unauthorized scope allowed")
	}
	bad := []Device{{ID: "x", Ed: Key{}, X: Key{}}}
	if err := other.Restore(bad, nil, now); err == nil {
		t.Fatal("invalid device restored")
	}
}
