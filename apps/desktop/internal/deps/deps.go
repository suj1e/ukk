//go:build deps

// Package deps pins modules that are only imported by build-tagged code paths.
// It keeps `go mod tidy` from dropping them while their packages are developed
// in parallel. It is never built into the application.
package deps

import (
	_ "github.com/coder/websocket"
	_ "github.com/google/uuid"
	_ "github.com/pion/webrtc/v4"
	_ "github.com/skip2/go-qrcode"
	_ "golang.org/x/crypto/chacha20poly1305"
	_ "golang.org/x/crypto/curve25519"
	_ "golang.org/x/crypto/ed25519"
	_ "golang.org/x/crypto/hkdf"
	_ "golang.org/x/sys/unix"
)
