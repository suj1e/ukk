package world

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// viewState is the private, machine-local selection state. It is deliberately
// not part of the herdr-app document: the document is shared through
// herdr-core, while which space, tab and pane this client is looking at is
// local UI state.
type viewState struct {
	ActiveSpace  string            `json:"active_space,omitempty"`
	ActiveTabs   map[string]string `json:"active_tabs,omitempty"`
	FocusedPanes map[string]string `json:"focused_panes,omitempty"`
}

func newViewState() viewState {
	return viewState{
		ActiveTabs:   map[string]string{},
		FocusedPanes: map[string]string{},
	}
}

func cloneViewState(v viewState) viewState {
	out := viewState{ActiveSpace: v.ActiveSpace}
	if len(v.ActiveTabs) > 0 {
		out.ActiveTabs = make(map[string]string, len(v.ActiveTabs))
		for k, value := range v.ActiveTabs {
			out.ActiveTabs[k] = value
		}
	}
	if len(v.FocusedPanes) > 0 {
		out.FocusedPanes = make(map[string]string, len(v.FocusedPanes))
		for k, value := range v.FocusedPanes {
			out.FocusedPanes[k] = value
		}
	}
	return out
}

// readView reads the view state at path. A missing file is not an error and
// returns nil so the caller keeps the derived defaults.
func readView(path string) (*viewState, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	var v viewState
	if err := json.Unmarshal(data, &v); err != nil {
		return nil, fmt.Errorf("decode %s: %w", path, err)
	}
	return &v, nil
}

// SaveView atomically writes the private view state to viewPath. The file is
// created with mode 0600 and its directory with mode 0700.
func (w *World) SaveView() error {
	w.mu.Lock()
	path := w.viewPath
	v := cloneViewState(w.view)
	w.mu.Unlock()

	if path == "" {
		return fmt.Errorf("%w: no view path", ErrView)
	}
	data, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("%w: encode view: %v", ErrView, err)
	}
	if err := writeViewFile(path, data); err != nil {
		return fmt.Errorf("%w: %v", ErrView, err)
	}
	return nil
}

func writeViewFile(path string, data []byte) error {
	dir := filepath.Dir(path)
	_, statErr := os.Stat(dir)
	if os.IsNotExist(statErr) {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
		// MkdirAll applies the mode subject to umask; make it exact.
		if err := os.Chmod(dir, 0o700); err != nil {
			return err
		}
	} else if statErr != nil {
		return statErr
	}
	tmp, err := os.CreateTemp(dir, ".world-view-*")
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
	if _, err := tmp.Write(data); err != nil {
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
	return nil
}
