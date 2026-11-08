package extensions

import (
	"os"
	"testing"
)

// testDir ignores a cleanup failure: Windows can keep a helper executable locked
// for a moment after the supervisor kills it.
func testDir(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "ukk-extensions-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}
