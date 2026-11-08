//go:build !darwin && !windows && !linux

package remotescreen

import (
	"fmt"
	"runtime"
)

// newPlatformInput reports an explicit unsupported platform instead of
// pretending input injection is available.
func newPlatformInput() (Input, error) {
	return nil, fmt.Errorf("%w: %w", ErrInputUnavailable, &UnsupportedPlatformError{Platform: runtime.GOOS})
}
