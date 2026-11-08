//go:build darwin

package remotescreen

import (
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func TestFindInputHelperMissing(t *testing.T) {
	t.Setenv("UKK_INPUT_HELPER", filepath.Join(t.TempDir(), "missing-ukk-input"))
	_, err := findInputHelper()
	if !errors.Is(err, ErrInputHelperMissing) {
		t.Fatalf("err = %v, want ErrInputHelperMissing", err)
	}
}

func TestFindInputHelperRequiresExecutable(t *testing.T) {
	path := filepath.Join(t.TempDir(), "ukk-input")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Setenv("UKK_INPUT_HELPER", path)
	if _, err := findInputHelper(); !errors.Is(err, ErrInputHelperMissing) {
		t.Fatalf("non-executable helper = %v", err)
	}
	if err := os.Chmod(path, 0o755); err != nil {
		t.Fatal(err)
	}
	got, err := findInputHelper()
	if err != nil {
		t.Fatalf("executable helper: %v", err)
	}
	if got != path {
		t.Fatalf("path = %q, want %q", got, path)
	}
}
