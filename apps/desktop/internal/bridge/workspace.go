package bridge

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
)

// MaxWorkspaceMirror bounds the metadata a Desktop may publish.
const MaxWorkspaceMirror = 1 << 20

// WorkspaceMirror is the read-only metadata copy served to remote clients. The
// Desktop remains the owner; the bridge never mutates product metadata.
type WorkspaceMirror struct {
	mu   sync.RWMutex
	path string
	data json.RawMessage
}

// OpenWorkspaceMirror loads or creates the mirror.
func OpenWorkspaceMirror(dir string) (*WorkspaceMirror, error) {
	mirror := &WorkspaceMirror{path: filepath.Join(dir, "workspace.mirror.json")}
	raw, err := os.ReadFile(mirror.path)
	switch {
	case errors.Is(err, os.ErrNotExist):
		return mirror, nil
	case err != nil:
		return nil, fmt.Errorf("bridge: read workspace mirror: %w", err)
	}
	if len(raw) > MaxWorkspaceMirror {
		return nil, errors.New("bridge: workspace mirror exceeds limit")
	}
	if !json.Valid(raw) {
		return nil, errors.New("bridge: workspace mirror is not valid JSON")
	}
	mirror.data = raw
	return mirror, nil
}

// Snapshot returns the current metadata.
func (m *WorkspaceMirror) Snapshot() json.RawMessage {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if len(m.data) == 0 {
		return json.RawMessage(`{"spaces":[],"sessions":[],"agents":[],"layouts":{}}`)
	}
	return append(json.RawMessage(nil), m.data...)
}

// Replace validates and persists a new snapshot.
func (m *WorkspaceMirror) Replace(raw json.RawMessage) error {
	if len(raw) == 0 {
		return errors.New("empty workspace snapshot")
	}
	if len(raw) > MaxWorkspaceMirror {
		return errors.New("workspace snapshot exceeds 1 MiB")
	}
	if !json.Valid(raw) {
		return errors.New("workspace snapshot is not valid JSON")
	}
	var probe map[string]json.RawMessage
	if err := json.Unmarshal(raw, &probe); err != nil {
		return errors.New("workspace snapshot must be a JSON object")
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if err := writeAtomic(m.path, append(append([]byte(nil), raw...), '\n')); err != nil {
		return err
	}
	m.data = append(json.RawMessage(nil), raw...)
	return nil
}
