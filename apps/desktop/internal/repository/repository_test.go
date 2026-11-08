package repository

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func git(t *testing.T, root string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", root}, args...)...)
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=Test", "GIT_AUTHOR_EMAIL=test@example.com",
		"GIT_COMMITTER_NAME=Test", "GIT_COMMITTER_EMAIL=test@example.com",
		"GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_SYSTEM=/dev/null",
	)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

func fixture(t *testing.T) (*Repo, string) {
	t.Helper()
	root := t.TempDir()
	git(t, root, "init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(root, "README.md"), []byte("hello\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	git(t, root, "add", "README.md")
	git(t, root, "commit", "-q", "-m", "initial")
	if err := os.MkdirAll(filepath.Join(root, "src"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "src", "app.go"), []byte("package main\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	repo, err := Open(root, Compact())
	if err != nil {
		t.Fatal(err)
	}
	return repo, root
}

func TestStatusDiffAndActivity(t *testing.T) {
	repo, root := fixture(t)
	if err := os.WriteFile(filepath.Join(root, "README.md"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	status, err := repo.Status(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if status.Branch != "main" || len(status.Entries) != 2 {
		t.Fatalf("status = %+v", status)
	}
	foundUntracked := false
	for _, entry := range status.Entries {
		if entry.Path == "src/app.go" && entry.Untracked {
			foundUntracked = true
		}
	}
	if !foundUntracked {
		t.Fatalf("untracked file missing: %+v", status.Entries)
	}
	diff, err := repo.Diff(context.Background(), ScopeHead)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(diff, "-hello") || !strings.Contains(diff, "+changed") {
		t.Fatalf("diff = %q", diff)
	}
	commits, err := repo.Activity(context.Background(), 10)
	if err != nil || len(commits) != 1 || commits[0].Subject != "initial" {
		t.Fatalf("activity = %+v, %v", commits, err)
	}
}

func TestTreeAndReadRejectEscapes(t *testing.T) {
	repo, _ := fixture(t)
	nodes, err := repo.Tree(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, node := range nodes {
		seen[node.Path] = true
	}
	if !seen["README.md"] || !seen["src/app.go"] {
		t.Fatalf("tree = %+v", nodes)
	}
	data, err := repo.Read("src/app.go")
	if err != nil || string(data) != "package main\n" {
		t.Fatalf("read = %q, %v", data, err)
	}
	for _, path := range []string{"../secret", "/etc/passwd", "src/../..", "."} {
		if _, err := repo.Read(path); !errors.Is(err, ErrOutsideRoot) {
			t.Fatalf("Read(%q) error = %v", path, err)
		}
		if _, err := repo.DiffFile(context.Background(), path, ScopeHead); !errors.Is(err, ErrOutsideRoot) {
			t.Fatalf("DiffFile(%q) error = %v", path, err)
		}
	}
}

func TestOpenRejectsNonRepository(t *testing.T) {
	if _, err := Open(t.TempDir(), Compact()); !errors.Is(err, ErrNotRepository) {
		t.Fatalf("error = %v", err)
	}
}
