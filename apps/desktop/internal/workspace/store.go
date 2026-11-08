package workspace

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
)

type document struct {
	Revision int64                    `json:"revision"`
	Mutation uint64                   `json:"mutation"`
	Spaces   []Space                  `json:"spaces"`
	Sessions []Session                `json:"sessions"`
	Agents   []AgentRun               `json:"agents"`
	Layouts  map[string]SessionLayout `json:"layouts"`
}

// Store persists product state in one JSON document. Writes are atomic
// (temporary file, fsync, rename) and mutations are serialized.
type Store struct {
	mu     sync.Mutex
	path   string
	doc    document
	subs   map[chan struct{}]struct{}
	closed bool
}

// Open loads a store from dir, creating the directory when needed. A missing
// document is a new workspace; a corrupt document is an error, never silently
// discarded.
func Open(dir string) (*Store, error) {
	if strings.TrimSpace(dir) == "" {
		return nil, fmt.Errorf("%w: store directory required", ErrInvalid)
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("workspace: create directory: %w", err)
	}
	s := &Store{
		path: filepath.Join(dir, "workspace.json"),
		subs: make(map[chan struct{}]struct{}),
		doc: document{
			Revision: CurrentRevision,
			Layouts:  make(map[string]SessionLayout),
		},
	}
	raw, err := os.ReadFile(s.path)
	switch {
	case errors.Is(err, os.ErrNotExist):
		return s, nil
	case err != nil:
		return nil, fmt.Errorf("workspace: read: %w", err)
	}
	var loaded document
	if err := json.Unmarshal(raw, &loaded); err != nil {
		return nil, fmt.Errorf("workspace: decode %s: %w", s.path, err)
	}
	if loaded.Revision != CurrentRevision {
		return nil, fmt.Errorf("%w: unsupported document revision %d", ErrInvalid, loaded.Revision)
	}
	if loaded.Layouts == nil {
		loaded.Layouts = make(map[string]SessionLayout)
	}
	s.doc = loaded
	if err := s.validateAll(); err != nil {
		return nil, err
	}
	return s, nil
}

// Path returns the document path.
func (s *Store) Path() string { return s.path }

func (s *Store) validateAll() error {
	spaceIDs := make(map[string]bool, len(s.doc.Spaces))
	for _, space := range s.doc.Spaces {
		if space.ID == "" || spaceIDs[space.ID] {
			return fmt.Errorf("%w: duplicate or empty space id", ErrInvalid)
		}
		spaceIDs[space.ID] = true
	}
	sessionIDs := make(map[string]bool, len(s.doc.Sessions))
	for _, session := range s.doc.Sessions {
		if session.ID == "" || sessionIDs[session.ID] {
			return fmt.Errorf("%w: duplicate or empty session id", ErrInvalid)
		}
		if !spaceIDs[session.SpaceID] {
			return fmt.Errorf("%w: session %s names unknown space", ErrInvalid, session.ID)
		}
		sessionIDs[session.ID] = true
	}
	bound := make(map[string]string, len(s.doc.Agents))
	for _, agent := range s.doc.Agents {
		if agent.ID == "" {
			return fmt.Errorf("%w: empty agent run id", ErrInvalid)
		}
		if !sessionIDs[agent.SessionID] {
			return fmt.Errorf("%w: agent %s names unknown session", ErrInvalid, agent.ID)
		}
		if agent.CoreID == "" || agent.TerminalAddress == "" {
			return fmt.Errorf("%w: agent %s needs core id and terminal address", ErrInvalid, agent.ID)
		}
		key := agent.CoreID + "\x00" + agent.TerminalAddress
		if other, ok := bound[key]; ok && other != agent.ID {
			return fmt.Errorf("%w: terminal bound twice", ErrAlreadyBound)
		}
		bound[key] = agent.ID
	}
	for sessionID, layout := range s.doc.Layouts {
		if !sessionIDs[sessionID] {
			return fmt.Errorf("%w: layout names unknown session %s", ErrInvalid, sessionID)
		}
		if err := validateLayout(layout); err != nil {
			return err
		}
	}
	return nil
}

func validateLayout(layout SessionLayout) error {
	if layout.SessionID == "" {
		return fmt.Errorf("%w: layout session id required", ErrInvalid)
	}
	if len(layout.Tabs) == 0 {
		return fmt.Errorf("%w: layout needs at least one tab", ErrInvalid)
	}
	seen := map[string]bool{}
	activeFound := false
	for _, tab := range layout.Tabs {
		if tab.ID == "" || seen[tab.ID] {
			return fmt.Errorf("%w: duplicate or empty tab id", ErrInvalid)
		}
		seen[tab.ID] = true
		if tab.Title == "" {
			return fmt.Errorf("%w: tab title required", ErrInvalid)
		}
		if err := tab.Root.Validate(); err != nil {
			return err
		}
		if tab.ID == layout.ActiveTab {
			activeFound = true
		}
	}
	if layout.ActiveTab != "" && !activeFound {
		return fmt.Errorf("%w: active tab is not in the layout", ErrInvalid)
	}
	return nil
}

// DefaultLayout returns a layout with one conversation tab.
func DefaultLayout(sessionID string) SessionLayout {
	root := NewLeaf(SurfaceChat)
	return SessionLayout{
		SessionID: sessionID,
		Tabs:      []Tab{{ID: newID(), Title: "Session", Root: root}},
		ActiveTab: "",
	}
}

// Snapshot returns a deep copy of the durable state.
func (s *Store) Snapshot() Snapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := Snapshot{
		Revision: int64(s.doc.Mutation),
		Spaces:   append([]Space(nil), s.doc.Spaces...),
		Sessions: append([]Session(nil), s.doc.Sessions...),
		Agents:   append([]AgentRun(nil), s.doc.Agents...),
		Layouts:  make(map[string]SessionLayout, len(s.doc.Layouts)),
	}
	for id, layout := range s.doc.Layouts {
		out.Layouts[id] = copyLayout(layout)
	}
	sort.Slice(out.Spaces, func(i, j int) bool { return out.Spaces[i].CreatedMS < out.Spaces[j].CreatedMS })
	sort.Slice(out.Sessions, func(i, j int) bool { return out.Sessions[i].CreatedMS < out.Sessions[j].CreatedMS })
	sort.Slice(out.Agents, func(i, j int) bool { return out.Agents[i].CreatedMS < out.Agents[j].CreatedMS })
	return out
}

func copyLayout(layout SessionLayout) SessionLayout {
	out := layout
	out.Tabs = make([]Tab, len(layout.Tabs))
	for i, tab := range layout.Tabs {
		out.Tabs[i] = Tab{ID: tab.ID, Title: tab.Title, Root: copyNode(tab.Root)}
	}
	return out
}

func copyNode(node *PaneNode) *PaneNode {
	if node == nil {
		return nil
	}
	out := *node
	out.First = copyNode(node.First)
	out.Second = copyNode(node.Second)
	return &out
}

// Subscribe returns a channel notified after each committed mutation. Slow
// subscribers lose intermediate notifications rather than blocking writers.
func (s *Store) Subscribe() <-chan struct{} {
	s.mu.Lock()
	defer s.mu.Unlock()
	ch := make(chan struct{}, 1)
	if s.closed {
		close(ch)
		return ch
	}
	s.subs[ch] = struct{}{}
	return ch
}

func (s *Store) notifyLocked() {
	for ch := range s.subs {
		select {
		case ch <- struct{}{}:
		default:
		}
	}
}

// Close releases subscribers. It does not touch any Agent.
func (s *Store) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil
	}
	s.closed = true
	for ch := range s.subs {
		close(ch)
		delete(s.subs, ch)
	}
	return nil
}

// CreateSpace adds a Space.
func (s *Store) CreateSpace(title, projectRoot string) (Space, error) {
	return s.CreateSpaceWithID(newID(), title, projectRoot)
}

// CreateSpaceWithID is idempotent: the same id and inputs return the existing
// Space; the same id with different inputs is a conflict.
func (s *Store) CreateSpaceWithID(id, title, projectRoot string) (Space, error) {
	t, err := cleanTitle(title)
	if err != nil {
		return Space{}, err
	}
	root, err := cleanRoot(projectRoot)
	if err != nil {
		return Space{}, err
	}
	if id == "" {
		return Space{}, fmt.Errorf("%w: space id required", ErrInvalid)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, existing := range s.doc.Spaces {
		if existing.ID != id {
			continue
		}
		if existing.Title == t && existing.ProjectRoot == root {
			return existing, nil
		}
		return Space{}, ErrConflict
	}
	space := Space{ID: id, Title: t, ProjectRoot: root, CreatedMS: nowMS()}
	s.doc.Spaces = append(s.doc.Spaces, space)
	if err := s.commitLocked(); err != nil {
		s.doc.Spaces = s.doc.Spaces[:len(s.doc.Spaces)-1]
		return Space{}, err
	}
	return space, nil
}

// RenameSpace changes only the title.
func (s *Store) RenameSpace(id, title string) (Space, error) {
	t, err := cleanTitle(title)
	if err != nil {
		return Space{}, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.doc.Spaces {
		if s.doc.Spaces[i].ID == id {
			previous := s.doc.Spaces[i].Title
			s.doc.Spaces[i].Title = t
			if err := s.commitLocked(); err != nil {
				s.doc.Spaces[i].Title = previous
				return Space{}, err
			}
			return s.doc.Spaces[i], nil
		}
	}
	return Space{}, ErrNotFound
}

// CreateSession adds a Session under a Space with a default layout.
func (s *Store) CreateSession(spaceID, title string) (Session, error) {
	t, err := cleanTitle(title)
	if err != nil {
		return Session{}, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	found := false
	for _, space := range s.doc.Spaces {
		if space.ID == spaceID {
			found = true
			break
		}
	}
	if !found {
		return Session{}, ErrNotFound
	}
	now := nowMS()
	session := Session{
		ID: newID(), SpaceID: spaceID, Title: t,
		Lifecycle: LifecycleActive, CreatedMS: now, UpdatedMS: now,
	}
	layout := DefaultLayout(session.ID)
	layout.ActiveTab = layout.Tabs[0].ID
	layout.UpdatedMS = now
	s.doc.Sessions = append(s.doc.Sessions, session)
	s.doc.Layouts[session.ID] = layout
	if err := s.commitLocked(); err != nil {
		s.doc.Sessions = s.doc.Sessions[:len(s.doc.Sessions)-1]
		delete(s.doc.Layouts, session.ID)
		return Session{}, err
	}
	return session, nil
}

// SetSessionLifecycle changes a Session's lifecycle without touching terminals.
func (s *Store) SetSessionLifecycle(id, lifecycle string) (Session, error) {
	switch lifecycle {
	case LifecycleActive, LifecyclePaused, LifecycleArchived:
	default:
		return Session{}, fmt.Errorf("%w: unknown lifecycle", ErrInvalid)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.doc.Sessions {
		if s.doc.Sessions[i].ID != id {
			continue
		}
		previous, previousUpdated := s.doc.Sessions[i].Lifecycle, s.doc.Sessions[i].UpdatedMS
		s.doc.Sessions[i].Lifecycle = lifecycle
		s.doc.Sessions[i].UpdatedMS = nowMS()
		if err := s.commitLocked(); err != nil {
			s.doc.Sessions[i].Lifecycle, s.doc.Sessions[i].UpdatedMS = previous, previousUpdated
			return Session{}, err
		}
		return s.doc.Sessions[i], nil
	}
	return Session{}, ErrNotFound
}

// BindAgent creates an AgentRun for a live terminal. Callers must verify live
// core identity before binding; this only enforces metadata integrity.
func (s *Store) BindAgent(sessionID, coreID, address, adapter, parentID string) (AgentRun, error) {
	if coreID == "" || address == "" {
		return AgentRun{}, fmt.Errorf("%w: core id and terminal address required", ErrInvalid)
	}
	if adapter == "" {
		adapter = "unknown"
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	sessionFound := false
	for _, session := range s.doc.Sessions {
		if session.ID == sessionID {
			sessionFound = true
			break
		}
	}
	if !sessionFound {
		return AgentRun{}, ErrNotFound
	}
	if parentID != "" {
		parentFound := false
		for _, agent := range s.doc.Agents {
			if agent.ID == parentID && agent.SessionID == sessionID {
				parentFound = true
				break
			}
		}
		if !parentFound {
			return AgentRun{}, fmt.Errorf("%w: parent run not in this session", ErrInvalid)
		}
	}
	for _, agent := range s.doc.Agents {
		if agent.CoreID == coreID && agent.TerminalAddress == address {
			return AgentRun{}, ErrAlreadyBound
		}
	}
	run := AgentRun{
		ID: newID(), SessionID: sessionID, ParentID: parentID,
		CoreID: coreID, TerminalAddress: address, Adapter: adapter,
		Status: AgentUnknown, CreatedMS: nowMS(),
	}
	s.doc.Agents = append(s.doc.Agents, run)
	if err := s.commitLocked(); err != nil {
		s.doc.Agents = s.doc.Agents[:len(s.doc.Agents)-1]
		return AgentRun{}, err
	}
	return run, nil
}

// SetAgentStatus records an observed status. It never starts or stops anything.
func (s *Store) SetAgentStatus(id, status string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.doc.Agents {
		if s.doc.Agents[i].ID == id {
			previous := s.doc.Agents[i].Status
			s.doc.Agents[i].Status = status
			if err := s.commitLocked(); err != nil {
				s.doc.Agents[i].Status = previous
				return err
			}
			return nil
		}
	}
	return ErrNotFound
}

// UnbindAgent removes only metadata.
func (s *Store) UnbindAgent(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.doc.Agents {
		if s.doc.Agents[i].ID == id {
			removed := s.doc.Agents[i]
			remaining := append(s.doc.Agents[:i], s.doc.Agents[i+1:]...)
			s.doc.Agents = remaining
			for j := range s.doc.Agents {
				if s.doc.Agents[j].ParentID == id {
					s.doc.Agents[j].ParentID = ""
				}
			}
			if err := s.commitLocked(); err != nil {
				s.doc.Agents = append(s.doc.Agents, removed)
				return err
			}
			return nil
		}
	}
	return ErrNotFound
}

// Layout returns a Session's layout.
func (s *Store) Layout(sessionID string) (SessionLayout, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	layout, ok := s.doc.Layouts[sessionID]
	return copyLayout(layout), ok
}

// SaveLayout replaces a Session's layout with compare-and-swap on the layout
// revision. A stale revision is a conflict; callers reload before retrying.
func (s *Store) SaveLayout(sessionID string, layout SessionLayout, expected uint64) (SessionLayout, error) {
	layout.SessionID = sessionID
	if err := validateLayout(layout); err != nil {
		return SessionLayout{}, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	current, ok := s.doc.Layouts[sessionID]
	if !ok {
		return SessionLayout{}, ErrNotFound
	}
	if expected != current.Revision {
		return SessionLayout{}, ErrConflict
	}
	next := copyLayout(layout)
	next.Revision = current.Revision + 1
	next.UpdatedMS = nowMS()
	previous := s.doc.Layouts[sessionID]
	s.doc.Layouts[sessionID] = next
	if err := s.commitLocked(); err != nil {
		s.doc.Layouts[sessionID] = previous
		return SessionLayout{}, err
	}
	return copyLayout(next), nil
}

func (s *Store) commitLocked() error {
	s.doc.Mutation++
	if err := s.persistLocked(); err != nil {
		s.doc.Mutation--
		return err
	}
	s.notifyLocked()
	return nil
}

func (s *Store) persistLocked() error {
	raw, err := json.MarshalIndent(s.doc, "", "  ")
	if err != nil {
		return fmt.Errorf("workspace: encode: %w", err)
	}
	raw = append(raw, '\n')
	dir := filepath.Dir(s.path)
	tmp, err := os.CreateTemp(dir, ".workspace-*.tmp")
	if err != nil {
		return fmt.Errorf("workspace: temporary file: %w", err)
	}
	name := tmp.Name()
	cleanup := func() {
		tmp.Close()
		os.Remove(name)
	}
	if err := tmp.Chmod(0o600); err != nil {
		cleanup()
		return fmt.Errorf("workspace: permission: %w", err)
	}
	if _, err := tmp.Write(raw); err != nil {
		cleanup()
		return fmt.Errorf("workspace: write: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		cleanup()
		return fmt.Errorf("workspace: sync: %w", err)
	}
	if err := tmp.Close(); err != nil {
		os.Remove(name)
		return fmt.Errorf("workspace: close: %w", err)
	}
	if err := os.Rename(name, s.path); err != nil {
		os.Remove(name)
		return fmt.Errorf("workspace: replace: %w", err)
	}
	if dirHandle, err := os.Open(dir); err == nil {
		_ = dirHandle.Sync()
		_ = dirHandle.Close()
	}
	return nil
}
