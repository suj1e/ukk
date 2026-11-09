package world

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
)

type fakeClient struct {
	value   json.RawMessage
	version uint64
	getErr  error
	setErr  error
	sets    int
}

func (f *fakeClient) DocGet(context.Context, string, string) (json.RawMessage, uint64, error) {
	if f.getErr != nil {
		return nil, 0, f.getErr
	}
	return append(json.RawMessage(nil), f.value...), f.version, nil
}

func (f *fakeClient) DocSet(_ context.Context, _ string, _ string, version uint64, value json.RawMessage) (uint64, error) {
	if f.setErr != nil {
		return 0, f.setErr
	}
	if version != f.version {
		return 0, fmt.Errorf("%w: stale version %d", ErrConflict, version)
	}
	f.sets++
	f.version++
	f.value = append(json.RawMessage(nil), value...)
	return f.version, nil
}

func block(id, terminal string) corelayout.Node {
	return corelayout.Node{
		ID:      id,
		Type:    corelayout.TypeBlock,
		Content: &corelayout.Content{Terminal: &terminal},
	}
}

func viewDocument() json.RawMessage {
	return json.RawMessage(`{"spaces":[
		{"id":"sp_1","name":"one","folder":"/tmp/one","tabs":[
			{"id":"tb_1","name":"main","root":{"id":"row_1","type":"row","children":[
				{"id":"p_1","type":"block","content":{"terminal":"t1"}},
				{"id":"p_2","type":"block","content":{"terminal":"t2"}}]}},
			{"id":"tb_2","name":"other","root":{"id":"p_3","type":"block","content":{"terminal":"t3"}}}]},
		{"id":"sp_2","name":"two","folder":"","tabs":[
			{"id":"tb_3","name":"third","root":{"id":"p_4","type":"block","content":{"terminal":"t4"}}}]}
	]}`)
}

func TestLoadEmptyDocument(t *testing.T) {
	w, err := Load(context.Background(), &fakeClient{}, "mac", "")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if got := w.Spaces(); len(got) != 0 {
		t.Fatalf("Spaces = %v, want empty", got)
	}
	if _, ok := w.ActiveSpace(); ok {
		t.Fatal("ActiveSpace reported a space for an empty document")
	}
	if _, ok := w.ActiveTab(); ok {
		t.Fatal("ActiveTab reported a tab for an empty document")
	}
	if doc := w.Document(); doc.Spaces == nil || len(doc.Spaces) != 0 {
		t.Fatalf("Document = %#v, want non-nil empty spaces", doc)
	}
}

func TestMutations(t *testing.T) {
	ctx := context.Background()
	w, err := Load(ctx, &fakeClient{}, "mac", "")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}

	if err := w.AddSpace(corelayout.SharedSpace{ID: "sp_1", Name: "one", Folder: "/tmp/one"}); err != nil {
		t.Fatalf("AddSpace: %v", err)
	}
	space, ok := w.ActiveSpace()
	if !ok || space.ID != "sp_1" {
		t.Fatalf("ActiveSpace = %#v, %v; want sp_1", space, ok)
	}

	if err := w.AddTab("sp_1", corelayout.SharedTab{ID: "tb_1", Name: "main", Root: block("p_1", "t1")}); err != nil {
		t.Fatalf("AddTab: %v", err)
	}
	tab, ok := w.ActiveTab()
	if !ok || tab.ID != "tb_1" {
		t.Fatalf("ActiveTab = %#v, %v; want tb_1", tab, ok)
	}

	if err := w.Split("sp_1", "tb_1", "p_1", "row", block("p_2", "t2")); err != nil {
		t.Fatalf("Split: %v", err)
	}
	doc := w.Document()
	root := doc.Spaces[0].Tabs[0].Root
	if root.Type != corelayout.TypeRow || len(root.Children) != 2 {
		t.Fatalf("root = %#v, want row with two panes", root)
	}
	if root.Children[0].ID != "p_1" || root.Children[1].ID != "p_2" {
		t.Fatalf("children = %q, %q; want p_1, p_2", root.Children[0].ID, root.Children[1].ID)
	}
	if !w.FocusPane("tb_1", "p_2") {
		t.Fatal("FocusPane(p_2) = false")
	}
	if w.FocusPane("tb_1", "missing") {
		t.Fatal("FocusPane(missing) = true")
	}

	if err := w.RenameSpace("sp_1", "renamed"); err != nil {
		t.Fatalf("RenameSpace: %v", err)
	}
	if err := w.RenameTab("sp_1", "tb_1", "renamed tab"); err != nil {
		t.Fatalf("RenameTab: %v", err)
	}
	doc = w.Document()
	if doc.Spaces[0].Name != "renamed" || doc.Spaces[0].Tabs[0].Name != "renamed tab" {
		t.Fatalf("rename did not stick: %#v", doc.Spaces[0])
	}

	if err := w.RemovePane("sp_1", "tb_1", "p_1"); err != nil {
		t.Fatalf("RemovePane: %v", err)
	}
	root = w.Document().Spaces[0].Tabs[0].Root
	if len(root.Children) != 1 || root.Children[0].ID != "p_2" {
		t.Fatalf("after RemovePane root = %#v", root)
	}
	if err := w.RemovePane("sp_1", "tb_1", "p_2"); !errors.Is(err, ErrInvalid) {
		t.Fatalf("RemovePane(last) error = %v, want ErrInvalid", err)
	}

	if err := w.RemoveTab("sp_1", "tb_1"); err != nil {
		t.Fatalf("RemoveTab: %v", err)
	}
	if _, ok := w.ActiveTab(); ok {
		t.Fatal("ActiveTab after removing the only tab = true")
	}
	if err := w.RemoveSpace("sp_1"); err != nil {
		t.Fatalf("RemoveSpace: %v", err)
	}
	if _, ok := w.ActiveSpace(); ok {
		t.Fatal("ActiveSpace after removing the only space = true")
	}

	if err := w.RemoveSpace("missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("RemoveSpace(missing) error = %v, want ErrNotFound", err)
	}
}

func TestSelectionFallbackAfterRemoval(t *testing.T) {
	w, err := Load(context.Background(), &fakeClient{value: viewDocument(), version: 3}, "mac", "")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !w.SelectTab("sp_1", "tb_2") {
		t.Fatal("SelectTab(tb_2) = false")
	}
	if tab, ok := w.ActiveTab(); !ok || tab.ID != "tb_2" {
		t.Fatalf("ActiveTab = %#v, %v; want tb_2", tab, ok)
	}

	if err := w.RemoveTab("sp_1", "tb_2"); err != nil {
		t.Fatalf("RemoveTab(tb_2): %v", err)
	}
	if tab, ok := w.ActiveTab(); !ok || tab.ID != "tb_1" {
		t.Fatalf("ActiveTab after removal = %#v, %v; want tb_1", tab, ok)
	}

	if !w.SelectSpace("sp_2") {
		t.Fatal("SelectSpace(sp_2) = false")
	}
	if err := w.RemoveSpace("sp_2"); err != nil {
		t.Fatalf("RemoveSpace(sp_2): %v", err)
	}
	if space, ok := w.ActiveSpace(); !ok || space.ID != "sp_1" {
		t.Fatalf("ActiveSpace after removal = %#v, %v; want sp_1", space, ok)
	}
}

func TestViewPersistence(t *testing.T) {
	path := filepath.Join(t.TempDir(), "view", "view.json")
	client := &fakeClient{value: viewDocument(), version: 3}
	w, err := Load(context.Background(), client, "mac", path)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !w.SelectTab("sp_1", "tb_2") {
		t.Fatal("SelectTab(tb_2) = false")
	}
	if !w.FocusPane("tb_1", "p_2") {
		t.Fatal("FocusPane(p_2) = false")
	}
	if !w.SelectSpace("sp_2") {
		t.Fatal("SelectSpace(sp_2) = false")
	}
	if err := w.SaveView(); err != nil {
		t.Fatalf("SaveView: %v", err)
	}

	saved := readViewFile(t, path)
	if saved.ActiveSpace != "sp_2" {
		t.Fatalf("active space = %q, want sp_2", saved.ActiveSpace)
	}
	if saved.ActiveTabs["sp_1"] != "tb_2" {
		t.Fatalf("active tab for sp_1 = %q, want tb_2", saved.ActiveTabs["sp_1"])
	}
	if saved.FocusedPanes["tb_1"] != "p_2" {
		t.Fatalf("focused pane for tb_1 = %q, want p_2", saved.FocusedPanes["tb_1"])
	}

	// Reloading the view must restore the selected space. Saving again proves
	// the focused pane survived the round trip: normalize would pick p_1.
	w2, err := Load(context.Background(), client, "mac", path)
	if err != nil {
		t.Fatalf("Load with view: %v", err)
	}
	if space, ok := w2.ActiveSpace(); !ok || space.ID != "sp_2" {
		t.Fatalf("reloaded ActiveSpace = %#v, %v; want sp_2", space, ok)
	}
	if err := w2.SaveView(); err != nil {
		t.Fatalf("SaveView after reload: %v", err)
	}
	if saved = readViewFile(t, path); saved.FocusedPanes["tb_1"] != "p_2" {
		t.Fatalf("focused pane after reload = %q, want p_2", saved.FocusedPanes["tb_1"])
	}
}

func TestCorruptViewKeepsDocument(t *testing.T) {
	path := filepath.Join(t.TempDir(), "view.json")
	if err := os.WriteFile(path, []byte("{not json"), 0o600); err != nil {
		t.Fatalf("write view: %v", err)
	}
	w, err := Load(context.Background(), &fakeClient{value: viewDocument(), version: 1}, "mac", path)
	if !errors.Is(err, ErrView) {
		t.Fatalf("Load error = %v, want ErrView", err)
	}
	if w == nil {
		t.Fatal("Load returned a nil world with a corrupt view")
	}
	if got := len(w.Spaces()); got != 2 {
		t.Fatalf("Spaces = %d, want the loaded document's 2 spaces", got)
	}
}

func TestSaveAndConflict(t *testing.T) {
	ctx := context.Background()
	client := &fakeClient{version: 7}
	w, err := Load(ctx, client, "mac", "")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if err := w.AddSpace(corelayout.SharedSpace{ID: "sp_1", Name: "one"}); err != nil {
		t.Fatalf("AddSpace: %v", err)
	}
	if err := w.Save(ctx); err != nil {
		t.Fatalf("Save: %v", err)
	}
	if client.version != 8 || client.sets != 1 {
		t.Fatalf("client version = %d, sets = %d; want 8, 1", client.version, client.sets)
	}
	if err := w.RenameSpace("sp_1", "renamed"); err != nil {
		t.Fatalf("RenameSpace: %v", err)
	}
	if err := w.Save(ctx); err != nil {
		t.Fatalf("second Save: %v", err)
	}
	if client.version != 9 {
		t.Fatalf("client version = %d, want 9", client.version)
	}

	client.version = 99
	if err := w.RenameSpace("sp_1", "again"); err != nil {
		t.Fatalf("RenameSpace: %v", err)
	}
	err = w.Save(ctx)
	if !errors.Is(err, ErrConflict) {
		t.Fatalf("Save stale version error = %v, want ErrConflict", err)
	}
	if space, ok := w.ActiveSpace(); !ok || space.Name != "again" {
		t.Fatalf("document changed after a conflicted save: %#v, %v", space, ok)
	}
}

func TestInvalidDocumentsAreRejected(t *testing.T) {
	if w, err := Load(context.Background(), &fakeClient{
		value: json.RawMessage(`{"spaces":[{"id":"","name":"x","folder":"","tabs":[]}]}`),
	}, "mac", ""); !errors.Is(err, ErrInvalid) || w != nil {
		t.Fatalf("Load invalid document = %v, %v; want nil, ErrInvalid", w, err)
	}

	w, err := Load(context.Background(), &fakeClient{}, "mac", "")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if err := w.AddSpace(corelayout.SharedSpace{Name: "no id"}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("AddSpace without id error = %v, want ErrInvalid", err)
	}
	if got := len(w.Spaces()); got != 0 {
		t.Fatalf("invalid AddSpace changed the document: %d spaces", got)
	}

	if err := w.AddSpace(corelayout.SharedSpace{ID: "sp_1", Name: "one"}); err != nil {
		t.Fatalf("AddSpace: %v", err)
	}
	if err := w.AddTab("sp_1", corelayout.SharedTab{ID: "tb_1", Root: block("p_1", "t1")}); err != nil {
		t.Fatalf("AddTab: %v", err)
	}
	if err := w.AddSpace(corelayout.SharedSpace{ID: "sp_2", Name: "two"}); err != nil {
		t.Fatalf("AddSpace sp_2: %v", err)
	}
	if err := w.AddTab("sp_2", corelayout.SharedTab{ID: "tb_1", Root: block("p_2", "t2")}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("AddTab duplicate id error = %v, want ErrInvalid", err)
	}
}

func TestGettersAreCopies(t *testing.T) {
	w, err := Load(context.Background(), &fakeClient{value: viewDocument(), version: 1}, "mac", "")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}

	doc := w.Document()
	doc.Spaces[0].Name = "mutated"
	doc.Spaces[0].Tabs[0].Name = "mutated tab"
	doc.Spaces[0].Tabs[0].Root.Children[0].ID = "mutated pane"
	doc.Spaces = doc.Spaces[:1]

	spaces := w.Spaces()
	spaces[0].Name = "mutated again"

	space, _ := w.ActiveSpace()
	space.Name = "mutated space"

	tab, _ := w.ActiveTab()
	tab.Name = "mutated tab copy"

	got := w.Document()
	if len(got.Spaces) != 2 {
		t.Fatalf("document spaces = %d, want 2", len(got.Spaces))
	}
	if got.Spaces[0].Name != "one" || got.Spaces[0].Tabs[0].Name != "main" {
		t.Fatalf("document was mutated through a getter: %#v", got.Spaces[0])
	}
	if got.Spaces[0].Tabs[0].Root.Children[0].ID != "p_1" {
		t.Fatalf("pane tree was mutated through a getter: %#v", got.Spaces[0].Tabs[0].Root)
	}
	if space, _ := w.ActiveSpace(); space.Name != "one" {
		t.Fatalf("ActiveSpace shares state: %q", space.Name)
	}
	if tab, _ := w.ActiveTab(); tab.Name != "main" {
		t.Fatalf("ActiveTab shares state: %q", tab.Name)
	}
}

func TestNewID(t *testing.T) {
	first := NewID("sp")
	second := NewID("sp")
	if !strings.HasPrefix(first, "sp_") || len(first) != len("sp_")+32 {
		t.Fatalf("NewID = %q, want sp_ followed by 32 hex digits", first)
	}
	if first == second {
		t.Fatalf("NewID returned a duplicate: %q", first)
	}
}

func readViewFile(t *testing.T, path string) viewState {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read view: %v", err)
	}
	var v viewState
	if err := json.Unmarshal(data, &v); err != nil {
		t.Fatalf("decode view: %v", err)
	}
	return v
}
