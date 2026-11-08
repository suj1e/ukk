//go:build windows

package hc

import (
	"testing"
	"time"

	"golang.org/x/sys/windows"
)

// stillActive is GetExitCodeProcess's value for a live process.
const stillActive = 259

func waitPIDGone(t *testing.T, pid int, limit time.Duration) {
	t.Helper()
	deadline := time.Now().Add(limit)
	for {
		handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, uint32(pid))
		if err != nil {
			// A closed handle is all this check can observe on Windows.
			return
		}
		var code uint32
		queryErr := windows.GetExitCodeProcess(handle, &code)
		_ = windows.CloseHandle(handle)
		if queryErr != nil || code != stillActive {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("helper pid %d still alive after %s", pid, limit)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
