//go:build !windows

package remotescreen

import (
	"os"
)

// terminateProcess asks ffmpeg to exit cleanly so it can flush and close the
// output pipe. Stop escalates to Kill after stopGracePeriod.
func terminateProcess(p *os.Process) {
	if p == nil {
		return
	}
	_ = p.Signal(os.Interrupt)
}
