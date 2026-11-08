//go:build !windows

package hc

import (
	"syscall"
	"testing"
	"time"
)

// waitPIDGone reports when a reaped child no longer exists.
func waitPIDGone(t *testing.T, pid int, limit time.Duration) {
	t.Helper()
	deadline := time.Now().Add(limit)
	for {
		err := syscall.Kill(pid, 0)
		if err == syscall.ESRCH {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("helper pid %d still alive after %s (err=%v)", pid, limit, err)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
