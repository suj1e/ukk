//go:build windows

package remotescreen

import "os"

// terminateProcess uses Kill on Windows because os.Interrupt is not
// implemented for child processes. Stop still waits for the reaped process
// and returns the kill result.
func terminateProcess(p *os.Process) {
	if p == nil {
		return
	}
	_ = p.Kill()
}
