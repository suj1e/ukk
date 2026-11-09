package workspace

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestSpaceSessionAndReopen(t *testing.T) {
	dir := t.TempDir()
	store, err := Open(dir)
	if err != nil {
		t.Fatal(err)
	}
	space, err := store.CreateSpace("  ukk  ", "/tmp/project")
	if err != nil {
		t.Fatal(err)
	}
	if space.Title != "ukk" {
		t.Fatalf("title = %q", space.Title)
	}
	if _, err := store.CreateSpace("", "/tmp/x"); !errors.Is(err, ErrInvalid) {
		t.Fatalf("empty title error = %v", err)
	}
	session, err := store.CreateSession(space.ID, "Implement")
	if err != nil {
		t.Fatal(err)
	}
	layout, ok := store.Layout(session.ID)
	if !ok || len(layout.Tabs) != 1 || layout.Tabs[0].Root.Kind != KindLeaf {
		t.Fatalf("layout = %+v ok=%v", layout, ok)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := Open(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	snapshot := reopened.Snapshot()
	if len(snapshot.Spaces) != 1 || len(snapshot.Sessions) != 1 {
		t.Fatalf("snapshot = %+v", snapshot)
	}
	if snapshot.Layouts[session.ID].Tabs[0].Root.ID != layout.Tabs[0].Root.ID {
		t.Fatal("layout did not survive reopen")
	}
	info, err := os.Stat(filepath.Join(dir, "workspace.json"))
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		t.Fatalf("document mode = %v", info.Mode())
	}
}

func TestCreateSpaceIdempotent(t *testing.T) {
	store, err := Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	first, err := store.CreateSpaceWithID("11111111-1111-4111-8111-111111111111", "ukk", "/p")
	if err != nil {
		t.Fatal(err)
	}
	second, err := store.CreateSpaceWithID(first.ID, "ukk", "/p")
	if err != nil || second.ID != first.ID {
		t.Fatalf("retry = %+v, %v", second, err)
	}
	if _, err := store.CreateSpaceWithID(first.ID, "other", "/p"); !errors.Is(err, ErrConflict) {
		t.Fatalf("conflict = %v", err)
	}
	if len(store.Snapshot().Spaces) != 1 {
		t.Fatal("duplicate space created")
	}
}

func TestLayoutCASAndValidation(t *testing.T) {
	store, err := Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	space, _ := store.CreateSpace("ukk", "/p")
	session, err := store.CreateSession(space.ID, "Work")
	if err != nil {
		t.Fatal(err)
	}
	layout, _ := store.Layout(session.ID)
	leaf := layout.Tabs[0].Root
	root, err := leaf.SplitLeaf(leaf.ID, SplitRow, 0.5, SurfaceTerm)
	if err != nil {
		t.Fatal(err)
	}
	layout.Tabs[0].Root = root
	saved, err := store.SaveLayout(session.ID, layout, layout.Revision)
	if err != nil {
		t.Fatal(err)
	}
	if saved.Revision != layout.Revision+1 {
		t.Fatalf("revision = %d", saved.Revision)
	}
	if _, err := store.SaveLayout(session.ID, layout, saved.Revision+1); !errors.Is(err, ErrConflict) {
		t.Fatalf("stale CAS = %v", err)
	}

	bad := saved
	bad.Tabs = append([]Tab(nil), saved.Tabs...)
	bad.Tabs[0].Root = NewSplit(SplitRow, 0.95, NewLeaf(SurfaceChat), NewLeaf(SurfaceFiles))
	if _, err := store.SaveLayout(session.ID, bad, 0); !errors.Is(err, ErrInvalid) {
		t.Fatalf("ratio validation = %v", err)
	}

	deep := NewLeaf(SurfaceChat)
	for range MaxDepth + 1 {
		deep = NewSplit(SplitRow, 0.5, deep, NewLeaf(SurfaceChat))
	}
	bad.Tabs[0].Root = deep
	if _, err := store.SaveLayout(session.ID, bad, 0); !errors.Is(err, ErrInvalid) {
		t.Fatalf("depth validation = %v", err)
	}
}

func TestTerminalPanePersistsAddress(t *testing.T) {
	dir := t.TempDir()
	store, err := Open(dir)
	if err != nil {
		t.Fatal(err)
	}
	space, err := store.CreateSpace("Desk", "/p")
	if err != nil {
		t.Fatal(err)
	}
	session, err := store.CreateSession(space.ID, "Shell")
	if err != nil {
		t.Fatal(err)
	}
	layout, _ := store.Layout(session.ID)
	layout.Tabs[0].Root = NewTerminalLeaf("t7")
	saved, err := store.SaveLayout(session.ID, layout, layout.Revision)
	if err != nil {
		t.Fatal(err)
	}
	if got := saved.Tabs[0].Root.TerminalAddress; got != "t7" {
		t.Fatalf("terminal address = %q", got)
	}
	if got := store.Snapshot().Layouts[session.ID].Tabs[0].Root.TerminalAddress; got != "t7" {
		t.Fatalf("snapshot terminal address = %q", got)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := Open(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	loaded, ok := reopened.Layout(session.ID)
	if !ok || loaded.Tabs[0].Root.TerminalAddress != "t7" {
		t.Fatalf("reloaded layout = %+v ok=%v", loaded, ok)
	}
	if err := loaded.Tabs[0].Root.Validate(); err != nil {
		t.Fatalf("terminal leaf validation = %v", err)
	}
}

func TestCloseLeafPromotesSibling(t *testing.T) {
	left := NewLeaf(SurfaceChat)
	right := NewLeaf(SurfaceTerm)
	root := NewSplit(SplitRow, 0.4, left, right)
	next, err := root.CloseLeaf(left.ID)
	if err != nil {
		t.Fatal(err)
	}
	if next.ID != right.ID || next.Kind != KindLeaf {
		t.Fatalf("next = %+v", next)
	}
	only, err := right.CloseLeaf(right.ID)
	if err != nil {
		t.Fatal(err)
	}
	if only != nil {
		t.Fatalf("closing the last leaf = %+v", only)
	}
	if _, err := root.CloseLeaf("missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing leaf = %v", err)
	}
}

func TestAgentBindAndArchiveKeepingTerminal(t *testing.T) {
	store, err := Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	space, _ := store.CreateSpace("ukk", "/p")
	session, _ := store.CreateSession(space.ID, "Work")
	run, err := store.BindAgent(session.ID, "core-1", "t1", "omp", "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.BindAgent(session.ID, "core-1", "t1", "omp", ""); !errors.Is(err, ErrAlreadyBound) {
		t.Fatalf("duplicate bind = %v", err)
	}
	if _, err := store.BindAgent(session.ID, "core-2", "t1", "omp", run.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := store.BindAgent(session.ID, "core-3", "t9", "omp", "missing"); !errors.Is(err, ErrInvalid) {
		t.Fatalf("bad parent = %v", err)
	}
	if _, err := store.SetSessionLifecycle(session.ID, LifecycleArchived); err != nil {
		t.Fatal(err)
	}
	snapshot := store.Snapshot()
	if len(snapshot.Agents) != 2 {
		t.Fatalf("archiving removed agent metadata: %+v", snapshot.Agents)
	}
	if err := store.UnbindAgent(run.ID); err != nil {
		t.Fatal(err)
	}
	snapshot = store.Snapshot()
	if len(snapshot.Agents) != 1 || snapshot.Agents[0].ParentID != "" {
		t.Fatalf("unbind = %+v", snapshot.Agents)
	}
}

func TestCorruptDocumentIsNotSilentlyDiscarded(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "workspace.json"), []byte("{"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(dir); err == nil {
		t.Fatal("corrupt document opened without error")
	}
}

func TestSubscriptionNotifies(t *testing.T) {
	store, err := Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	ch := store.Subscribe()
	if _, err := store.CreateSpace("ukk", "/p"); err != nil {
		t.Fatal(err)
	}
	select {
	case <-ch:
	default:
		t.Fatal("no notification after mutation")
	}
	store.Close()
	if _, ok := <-ch; ok {
		t.Fatal("subscription not closed")
	}
}
