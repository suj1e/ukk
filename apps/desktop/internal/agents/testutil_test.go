package agents

import (
	"os"
	"testing"
)

// testDir ignores a cleanup failure: Windows can briefly keep a child's working
// directory or executable locked after the process exits.
func testDir(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "ukk-agents-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}
