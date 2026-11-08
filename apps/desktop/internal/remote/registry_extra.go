package remote

import (
	"sort"
	"time"
)

// Snapshot returns every registered device and grant for persistence. Keys and
// grants only; no channel material.
func (r *Registry) Snapshot() ([]Device, []Grant) {
	r.mu.Lock()
	defer r.mu.Unlock()
	devices := make([]Device, 0, len(r.devices))
	var grants []Grant
	for _, state := range r.devices {
		devices = append(devices, state.device)
		for _, grant := range state.grants {
			grants = append(grants, grant)
		}
	}
	sort.Slice(devices, func(i, j int) bool { return devices[i].CreatedAt.Before(devices[j].CreatedAt) })
	sort.Slice(grants, func(i, j int) bool {
		if grants[i].DeviceID != grants[j].DeviceID {
			return grants[i].DeviceID < grants[j].DeviceID
		}
		if grants[i].Scope != grants[j].Scope {
			return grants[i].Scope < grants[j].Scope
		}
		return grants[i].Subject < grants[j].Subject
	})
	return devices, grants
}

// Restore installs persisted devices and grants. Expired grants are kept so the
// app can show them and let them lapse; authorization still enforces expiry.
func (r *Registry) Restore(devices []Device, grants []Grant, now time.Time) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.devices = make(map[string]*deviceState, len(devices))
	for _, device := range devices {
		if !validIdentifier(device.ID) || device.Ed.IsZero() || device.X.IsZero() {
			return newError(CodeGrantInvalid, "invalid persisted device")
		}
		if device.Generation == 0 {
			device.Generation = 1
		}
		if device.CreatedAt.IsZero() {
			device.CreatedAt = now
		}
		r.devices[device.ID] = &deviceState{device: device, grants: map[grantKey]Grant{}}
	}
	for _, grant := range grants {
		if !grant.Scope.Valid() || !validSubject(grant.Subject) {
			return newError(CodeGrantInvalid, "invalid persisted grant")
		}
		state, ok := r.devices[grant.DeviceID]
		if !ok {
			return newError(CodeUnknownDevice, "persisted grant names an unknown device")
		}
		state.grants[grantKey{scope: grant.Scope, subject: grant.Subject}] = grant
	}
	return nil
}

// LookupByEd finds a device by its Ed25519 public key.
func (r *Registry) LookupByEd(ed Key) (Device, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, state := range r.devices {
		if state.device.Ed == ed {
			return state.device, true
		}
	}
	return Device{}, false
}

// Remove deletes a device and its grants, for a fresh pairing after an
// explicit host confirmation.
func (r *Registry) Remove(deviceID string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.devices, deviceID)
}
