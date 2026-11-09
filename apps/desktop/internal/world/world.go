// Package world owns the machine-owned herdr-app document and the private
// view state layered on top of it.
//
// The document (spaces, tabs and pane trees) is shared with herdr-core through
// doc.get/doc.set and is therefore machine-owned. Which space, tab and pane
// the local UI is looking at is user-local state and is kept in a separate
// view file; it is never written into the document.
//
// All document and view access is serialized by one mutex. Mutations build a
// deep copy of the document, validate it with corelayout, and only then swap it
// in, so an invalid mutation never leaves a half-applied document behind.
// Closing a pane, tab or space only changes this metadata: no terminal process
// is started, stopped or killed here.
package world

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"sync"

	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
	"github.com/suj1e/ukk/apps/desktop/internal/hc"
)

// DocumentName is the herdr-core document this package reads and writes.
const DocumentName = "herdr-app"

var (
	// ErrInvalid marks a mutation or document that corelayout rejects.
	ErrInvalid = corelayout.ErrInvalid
	// ErrNotFound marks a space, tab or pane that does not exist.
	ErrNotFound = errors.New("world: not found")
	// ErrConflict marks a stale doc.set. The caller must reload the document.
	ErrConflict = errors.New("world: conflict")
	// ErrView marks an unreadable or unwritable private view file. Load still
	// returns the world so the caller can keep working with the document.
	ErrView = errors.New("world: view")
)

// Client is the subset of hc.Client used by World.
type Client interface {
	DocGet(ctx context.Context, name, on string) (json.RawMessage, uint64, error)
	DocSet(ctx context.Context, name, on string, version uint64, value json.RawMessage) (uint64, error)
}

// World is a loaded document plus its local selection state.
type World struct {
	mu sync.Mutex

	client   Client
	machine  string
	viewPath string

	doc     corelayout.Document
	version uint64
	// gen counts committed document mutations. Save compares it to detect a
	// mutation that happened while doc.set was in flight.
	gen  uint64
	view viewState
}

// Load reads the machine document and, when viewPath names an existing file,
// its private view state. A missing document is an empty document. A corrupt
// view file is reported as an error wrapping ErrView, but the returned World
// is usable and keeps the loaded document.
func Load(ctx context.Context, client Client, machine, viewPath string) (*World, error) {
	if client == nil {
		return nil, fmt.Errorf("%w: nil client", ErrInvalid)
	}
	value, version, err := client.DocGet(ctx, DocumentName, machine)
	if err != nil {
		return nil, fmt.Errorf("world: load %s: %w", DocumentName, err)
	}

	doc := emptyDocument()
	if len(value) > 0 && !isNullJSON(value) {
		doc = corelayout.Document{}
		if err := json.Unmarshal(value, &doc); err != nil {
			return nil, fmt.Errorf("%w: decode document: %v", ErrInvalid, err)
		}
		if doc.Spaces == nil {
			doc.Spaces = []corelayout.SharedSpace{}
		}
	}
	if err := doc.Validate(); err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalid, err)
	}

	w := &World{
		client:   client,
		machine:  machine,
		viewPath: viewPath,
		doc:      doc,
		version:  version,
		view:     newViewState(),
	}
	w.normalizeViewLocked()

	if viewPath == "" {
		return w, nil
	}
	view, err := readView(viewPath)
	if err != nil {
		return w, fmt.Errorf("%w: %v", ErrView, err)
	}
	if view != nil {
		w.view = ensureView(*view)
		w.normalizeViewLocked()
	}
	return w, nil
}

// Spaces returns a deep copy of the document's spaces.
func (w *World) Spaces() []corelayout.SharedSpace {
	w.mu.Lock()
	defer w.mu.Unlock()
	return cloneDocument(w.doc).Spaces
}

// Document returns a deep copy of the whole document.
func (w *World) Document() corelayout.Document {
	w.mu.Lock()
	defer w.mu.Unlock()
	return cloneDocument(w.doc)
}

// ActiveSpace returns the selected space, falling back to the first space when
// the stored selection no longer exists.
func (w *World) ActiveSpace() (corelayout.SharedSpace, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	doc := cloneDocument(w.doc)
	if len(doc.Spaces) == 0 {
		return corelayout.SharedSpace{}, false
	}
	index := spaceIndex(&doc, w.view.ActiveSpace)
	if index < 0 {
		index = 0
	}
	return doc.Spaces[index], true
}

// ActiveTab returns the selected tab of the active space, falling back to that
// space's first tab.
func (w *World) ActiveTab() (corelayout.SharedTab, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	doc := cloneDocument(w.doc)
	if len(doc.Spaces) == 0 {
		return corelayout.SharedTab{}, false
	}
	index := spaceIndex(&doc, w.view.ActiveSpace)
	if index < 0 {
		index = 0
	}
	space := &doc.Spaces[index]
	if len(space.Tabs) == 0 {
		return corelayout.SharedTab{}, false
	}
	if index := tabIndex(space, w.view.ActiveTabs[space.ID]); index >= 0 {
		return space.Tabs[index], true
	}
	return space.Tabs[0], true
}

// SelectSpace selects a space. It reports false when the space does not exist.
func (w *World) SelectSpace(id string) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	if spaceIndex(&w.doc, id) < 0 {
		return false
	}
	w.view.ActiveSpace = id
	w.normalizeViewLocked()
	return true
}

// SelectTab selects a tab, and the space containing it. It reports false when
// the tab does not belong to spaceID.
func (w *World) SelectTab(spaceID, tabID string) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	index := spaceIndex(&w.doc, spaceID)
	if index < 0 {
		return false
	}
	space := &w.doc.Spaces[index]
	if tabIndex(space, tabID) < 0 {
		return false
	}
	w.view = ensureView(w.view)
	w.view.ActiveSpace = spaceID
	w.view.ActiveTabs[spaceID] = tabID
	w.normalizeViewLocked()
	return true
}

// FocusPane records the focused pane in a tab. It reports false when the tab or
// pane does not exist.
// FocusedPane returns the private view's focused pane for a tab, or "" when
// this client has not focused one. The value is local, never in the document.
func (w *World) FocusedPane(tabID string) string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.view.FocusedPanes[tabID]
}

func (w *World) FocusPane(tabID, paneID string) bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	tab := findTabByID(&w.doc, tabID)
	if tab == nil {
		return false
	}
	node := findNode(&tab.Root, paneID)
	if node == nil || node.Type != corelayout.TypeBlock {
		return false
	}
	w.view = ensureView(w.view)
	w.view.FocusedPanes[tabID] = paneID
	return true
}

// AddSpace appends a space and selects it. The whole resulting document is
// validated before it is committed.
func (w *World) AddSpace(space corelayout.SharedSpace) error {
	return w.apply(func(doc *corelayout.Document, view *viewState) error {
		copy, err := cloneSpaceValue(space)
		if err != nil {
			return fmt.Errorf("%w: space: %v", ErrInvalid, err)
		}
		doc.Spaces = append(doc.Spaces, copy)
		view.ActiveSpace = copy.ID
		return nil
	})
}

// AddTab appends a tab to a space and selects it.
func (w *World) AddTab(spaceID string, tab corelayout.SharedTab) error {
	return w.apply(func(doc *corelayout.Document, view *viewState) error {
		index := spaceIndex(doc, spaceID)
		if index < 0 {
			return fmt.Errorf("%w: space %q", ErrNotFound, spaceID)
		}
		copy, err := cloneTabValue(tab)
		if err != nil {
			return fmt.Errorf("%w: tab: %v", ErrInvalid, err)
		}
		doc.Spaces[index].Tabs = append(doc.Spaces[index].Tabs, copy)
		view.ActiveSpace = spaceID
		if view.ActiveTabs == nil {
			view.ActiveTabs = map[string]string{}
		}
		view.ActiveTabs[spaceID] = copy.ID
		return nil
	})
}

// Split replaces a pane with a row or column containing the original pane and
// block. axis is "row" or "column".
func (w *World) Split(spaceID, tabID, paneID, axis string, block corelayout.Node) error {
	var nodeType string
	switch axis {
	case "row":
		nodeType = corelayout.TypeRow
	case "column":
		nodeType = corelayout.TypeColumn
	default:
		return fmt.Errorf("%w: split axis must be row or column", ErrInvalid)
	}
	return w.apply(func(doc *corelayout.Document, _ *viewState) error {
		tab := findTab(doc, spaceID, tabID)
		if tab == nil {
			return fmt.Errorf("%w: tab %q in space %q", ErrNotFound, tabID, spaceID)
		}
		target := findNode(&tab.Root, paneID)
		if target == nil {
			return fmt.Errorf("%w: pane %q", ErrNotFound, paneID)
		}
		if target.Type != corelayout.TypeBlock {
			return fmt.Errorf("%w: %q is not a pane", ErrInvalid, paneID)
		}
		blockCopy, err := cloneNodeValue(block)
		if err != nil {
			return fmt.Errorf("%w: block: %v", ErrInvalid, err)
		}
		original := *target
		*target = corelayout.Node{
			ID:       NewID("p"),
			Type:     nodeType,
			Children: []corelayout.Node{original, blockCopy},
		}
		return nil
	})
}

// RemovePane removes one pane from a tab. Removing the tab's only pane fails:
// use RemoveTab to close the tab instead.
func (w *World) RemovePane(spaceID, tabID, paneID string) error {
	return w.apply(func(doc *corelayout.Document, view *viewState) error {
		tab := findTab(doc, spaceID, tabID)
		if tab == nil {
			return fmt.Errorf("%w: tab %q in space %q", ErrNotFound, tabID, spaceID)
		}
		target := findNode(&tab.Root, paneID)
		if target == nil {
			return fmt.Errorf("%w: pane %q", ErrNotFound, paneID)
		}
		if target.Type != corelayout.TypeBlock {
			return fmt.Errorf("%w: %q is not a pane", ErrInvalid, paneID)
		}
		if countPanes(&tab.Root) <= 1 {
			return fmt.Errorf("%w: cannot remove a tab's only pane", ErrInvalid)
		}
		if !removeNode(&tab.Root, paneID) {
			return fmt.Errorf("%w: pane %q", ErrNotFound, paneID)
		}
		if view.FocusedPanes != nil {
			delete(view.FocusedPanes, tabID)
		}
		return nil
	})
}

// RemoveTab removes a tab and its layout metadata.
func (w *World) RemoveTab(spaceID, tabID string) error {
	return w.apply(func(doc *corelayout.Document, view *viewState) error {
		index := spaceIndex(doc, spaceID)
		if index < 0 {
			return fmt.Errorf("%w: space %q", ErrNotFound, spaceID)
		}
		space := &doc.Spaces[index]
		tab := tabIndex(space, tabID)
		if tab < 0 {
			return fmt.Errorf("%w: tab %q in space %q", ErrNotFound, tabID, spaceID)
		}
		space.Tabs = append(space.Tabs[:tab], space.Tabs[tab+1:]...)
		if view.ActiveTabs != nil {
			delete(view.ActiveTabs, spaceID)
		}
		if view.FocusedPanes != nil {
			delete(view.FocusedPanes, tabID)
		}
		return nil
	})
}

// RemoveSpace removes a space and all of its tabs.
func (w *World) RemoveSpace(spaceID string) error {
	return w.apply(func(doc *corelayout.Document, _ *viewState) error {
		index := spaceIndex(doc, spaceID)
		if index < 0 {
			return fmt.Errorf("%w: space %q", ErrNotFound, spaceID)
		}
		doc.Spaces = append(doc.Spaces[:index], doc.Spaces[index+1:]...)
		return nil
	})
}

// RenameSpace changes a space's name.
func (w *World) RenameSpace(spaceID, name string) error {
	return w.apply(func(doc *corelayout.Document, _ *viewState) error {
		index := spaceIndex(doc, spaceID)
		if index < 0 {
			return fmt.Errorf("%w: space %q", ErrNotFound, spaceID)
		}
		doc.Spaces[index].Name = name
		return nil
	})
}

// RenameTab changes a tab's name.
func (w *World) RenameTab(spaceID, tabID, name string) error {
	return w.apply(func(doc *corelayout.Document, _ *viewState) error {
		tab := findTab(doc, spaceID, tabID)
		if tab == nil {
			return fmt.Errorf("%w: tab %q in space %q", ErrNotFound, tabID, spaceID)
		}
		tab.Name = name
		return nil
	})
}

// Save writes the document with doc.set compare-and-swap. The snapshot and
// version are captured under the lock and the network call runs outside it.
//
// A successful doc.set advances the recorded version only when the document
// was not mutated while the call was in flight. Otherwise the local document
// is newer than what the core accepted: the version stays behind, and the next
// Save reports ErrConflict so the caller reloads. A stale version is reported
// as ErrConflict immediately.
func (w *World) Save(ctx context.Context) error {
	w.mu.Lock()
	snapshot := cloneDocument(w.doc)
	version := w.version
	gen := w.gen
	w.mu.Unlock()

	value, err := json.Marshal(snapshot)
	if err != nil {
		return fmt.Errorf("%w: encode document: %v", ErrInvalid, err)
	}
	newVersion, err := w.client.DocSet(ctx, DocumentName, w.machine, version, value)
	if err != nil {
		if isConflict(err) {
			return fmt.Errorf("%w: %v", ErrConflict, err)
		}
		return fmt.Errorf("world: save %s: %w", DocumentName, err)
	}

	w.mu.Lock()
	defer w.mu.Unlock()
	if w.gen == gen {
		w.version = newVersion
	}
	return nil
}

// NewID returns a fresh 16-byte random hex identifier with an "sp_" (space),
// "tb_" (tab) or "p_" (pane/node) prefix.
func NewID(prefix string) string {
	var buf [16]byte
	if _, err := rand.Read(buf[:]); err != nil {
		panic("world: crypto/rand: " + err.Error())
	}
	return prefix + "_" + hex.EncodeToString(buf[:])
}

// apply runs fn against a deep copy of the document and view, validates the
// resulting document, and only then commits it.
func (w *World) apply(fn func(doc *corelayout.Document, view *viewState) error) error {
	w.mu.Lock()
	defer w.mu.Unlock()

	doc := cloneDocument(w.doc)
	view := ensureView(cloneViewState(w.view))
	if err := fn(&doc, &view); err != nil {
		return err
	}
	if err := doc.Validate(); err != nil {
		return fmt.Errorf("%w: %v", ErrInvalid, err)
	}
	if _, err := json.Marshal(doc); err != nil {
		return fmt.Errorf("%w: cannot encode document: %v", ErrInvalid, err)
	}

	w.doc = doc
	w.view = view
	w.gen++
	w.normalizeViewLocked()
	return nil
}

// normalizeViewLocked drops stale selections and fills missing ones from the
// document order.
func (w *World) normalizeViewLocked() {
	v := &w.view
	if v.ActiveTabs == nil {
		v.ActiveTabs = map[string]string{}
	}
	if v.FocusedPanes == nil {
		v.FocusedPanes = map[string]string{}
	}
	if len(w.doc.Spaces) == 0 {
		v.ActiveSpace = ""
		clear(v.ActiveTabs)
		clear(v.FocusedPanes)
		return
	}
	if spaceIndex(&w.doc, v.ActiveSpace) < 0 {
		v.ActiveSpace = w.doc.Spaces[0].ID
	}

	tabs := make(map[string]struct{})
	for i := range w.doc.Spaces {
		space := &w.doc.Spaces[i]
		if len(space.Tabs) == 0 {
			delete(v.ActiveTabs, space.ID)
			continue
		}
		if tabIndex(space, v.ActiveTabs[space.ID]) < 0 {
			v.ActiveTabs[space.ID] = space.Tabs[0].ID
		}
		for j := range space.Tabs {
			tab := &space.Tabs[j]
			tabs[tab.ID] = struct{}{}
			if !paneExists(&tab.Root, v.FocusedPanes[tab.ID]) {
				if id, ok := firstPaneID(&tab.Root); ok {
					v.FocusedPanes[tab.ID] = id
				} else {
					delete(v.FocusedPanes, tab.ID)
				}
			}
		}
	}
	for id := range v.ActiveTabs {
		if spaceIndex(&w.doc, id) < 0 {
			delete(v.ActiveTabs, id)
		}
	}
	for id := range v.FocusedPanes {
		if _, ok := tabs[id]; !ok {
			delete(v.FocusedPanes, id)
		}
	}
}

func emptyDocument() corelayout.Document {
	return corelayout.Document{Spaces: []corelayout.SharedSpace{}}
}

func ensureView(v viewState) viewState {
	if v.ActiveTabs == nil {
		v.ActiveTabs = map[string]string{}
	}
	if v.FocusedPanes == nil {
		v.FocusedPanes = map[string]string{}
	}
	return v
}

func spaceIndex(doc *corelayout.Document, id string) int {
	for i := range doc.Spaces {
		if doc.Spaces[i].ID == id {
			return i
		}
	}
	return -1
}

func tabIndex(space *corelayout.SharedSpace, id string) int {
	for i := range space.Tabs {
		if space.Tabs[i].ID == id {
			return i
		}
	}
	return -1
}

func findTab(doc *corelayout.Document, spaceID, tabID string) *corelayout.SharedTab {
	index := spaceIndex(doc, spaceID)
	if index < 0 {
		return nil
	}
	space := &doc.Spaces[index]
	tab := tabIndex(space, tabID)
	if tab < 0 {
		return nil
	}
	return &space.Tabs[tab]
}

func findTabByID(doc *corelayout.Document, tabID string) *corelayout.SharedTab {
	for i := range doc.Spaces {
		space := &doc.Spaces[i]
		if index := tabIndex(space, tabID); index >= 0 {
			return &space.Tabs[index]
		}
	}
	return nil
}

func findNode(node *corelayout.Node, id string) *corelayout.Node {
	if node == nil {
		return nil
	}
	if node.ID == id {
		return node
	}
	for i := range node.Children {
		if found := findNode(&node.Children[i], id); found != nil {
			return found
		}
	}
	return nil
}

func removeNode(node *corelayout.Node, id string) bool {
	for i := range node.Children {
		if node.Children[i].ID == id {
			node.Children = append(node.Children[:i], node.Children[i+1:]...)
			return true
		}
		if removeNode(&node.Children[i], id) {
			return true
		}
	}
	return false
}

func countPanes(node *corelayout.Node) int {
	if node == nil {
		return 0
	}
	count := 0
	if node.Type == corelayout.TypeBlock {
		count++
	}
	for i := range node.Children {
		count += countPanes(&node.Children[i])
	}
	return count
}

func paneExists(node *corelayout.Node, id string) bool {
	if id == "" {
		return false
	}
	found := findNode(node, id)
	return found != nil && found.Type == corelayout.TypeBlock
}

func firstPaneID(node *corelayout.Node) (string, bool) {
	if node == nil {
		return "", false
	}
	if node.Type == corelayout.TypeBlock && node.ID != "" {
		return node.ID, true
	}
	for i := range node.Children {
		if id, ok := firstPaneID(&node.Children[i]); ok {
			return id, true
		}
	}
	return "", false
}

func isNullJSON(raw json.RawMessage) bool {
	return len(raw) == 0 || bytes.Equal(bytes.TrimSpace(raw), []byte("null"))
}

func isConflict(err error) bool {
	if errors.Is(err, ErrConflict) {
		return true
	}
	var hcErr *hc.Error
	return errors.As(err, &hcErr) && hcErr.Kind == "conflict"
}

func cloneDocument(doc corelayout.Document) corelayout.Document {
	data, err := json.Marshal(doc)
	if err == nil {
		var out corelayout.Document
		if err := json.Unmarshal(data, &out); err == nil {
			if out.Spaces == nil {
				out.Spaces = []corelayout.SharedSpace{}
			}
			return out
		}
	}
	// The document was validated before it was committed; this path only
	// exists so a getter never aliases internally owned slices.
	out := doc
	if doc.Spaces != nil {
		out.Spaces = append([]corelayout.SharedSpace(nil), doc.Spaces...)
	}
	return out
}

func cloneSpaceValue(space corelayout.SharedSpace) (corelayout.SharedSpace, error) {
	data, err := json.Marshal(space)
	if err != nil {
		return corelayout.SharedSpace{}, err
	}
	var out corelayout.SharedSpace
	if err := json.Unmarshal(data, &out); err != nil {
		return corelayout.SharedSpace{}, err
	}
	return out, nil
}

func cloneTabValue(tab corelayout.SharedTab) (corelayout.SharedTab, error) {
	data, err := json.Marshal(tab)
	if err != nil {
		return corelayout.SharedTab{}, err
	}
	var out corelayout.SharedTab
	if err := json.Unmarshal(data, &out); err != nil {
		return corelayout.SharedTab{}, err
	}
	return out, nil
}

func cloneNodeValue(node corelayout.Node) (corelayout.Node, error) {
	data, err := json.Marshal(node)
	if err != nil {
		return corelayout.Node{}, err
	}
	var out corelayout.Node
	if err := json.Unmarshal(data, &out); err != nil {
		return corelayout.Node{}, err
	}
	return out, nil
}
