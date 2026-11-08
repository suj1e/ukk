//go:build !windows

package bridge

import (
	"os/exec"
	"syscall"
)

// detach starts the bridge in its own session so the Desktop can exit without
// sending it a hangup.
func detach(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
}
