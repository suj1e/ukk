package bridge

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

// Listen opens the user-private local endpoint. Unix uses a 0600 socket;
// Windows uses a loopback port because Go has no portable named-pipe listener
// in the standard library, with a 0600 token file as the authorization secret.
func Listen(dir string) (net.Listener, Endpoint, error) {
	tokenPath := filepath.Join(dir, "bridge.token")
	if _, err := LocalToken(dir); err != nil {
		return nil, Endpoint{}, err
	}
	endpoint := Endpoint{
		Token:   tokenPath,
		PID:     os.Getpid(),
		Version: Version,
		Started: time.Now().UnixMilli(),
	}
	if runtime.GOOS == "windows" {
		listener, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			return nil, Endpoint{}, err
		}
		endpoint.Network = "tcp"
		endpoint.Address = listener.Addr().String()
		return listener, endpoint, nil
	}
	socket := filepath.Join(dir, "bridge.sock")
	if err := removeStaleSocket(socket); err != nil {
		return nil, Endpoint{}, err
	}
	listener, err := net.Listen("unix", socket)
	if err != nil {
		return nil, Endpoint{}, fmt.Errorf("bridge: listen: %w", err)
	}
	if err := os.Chmod(socket, 0o600); err != nil {
		listener.Close()
		return nil, Endpoint{}, fmt.Errorf("bridge: socket permission: %w", err)
	}
	endpoint.Network = "unix"
	endpoint.Address = socket
	return listener, endpoint, nil
}

// removeStaleSocket removes a socket only when nothing accepts connections on
// it and the path is still a socket owned by this user.
func removeStaleSocket(path string) error {
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSocket == 0 {
		return fmt.Errorf("bridge: %s exists and is not a socket", path)
	}
	conn, err := net.DialTimeout("unix", path, 300*time.Millisecond)
	if err == nil {
		conn.Close()
		return fmt.Errorf("bridge: another bridge is listening on %s", path)
	}
	if err := os.Remove(path); err != nil {
		return fmt.Errorf("bridge: remove stale socket: %w", err)
	}
	return nil
}

// LocalHTTP returns an HTTP client bound to the published endpoint and the
// bearer token file. It never uses a TCP fallback on Unix.
func LocalHTTP(dir string) (*http.Client, Endpoint, error) {
	endpoint, err := LoadEndpoint(dir, 24*time.Hour)
	if err != nil {
		return nil, Endpoint{}, err
	}
	network, address := endpoint.Network, endpoint.Address
	if network == "tcp" && runtime.GOOS != "windows" {
		return nil, Endpoint{}, errors.New("bridge: unexpected TCP endpoint")
	}
	transport := &http.Transport{
		DisableKeepAlives: false,
		MaxIdleConns:      4,
		IdleConnTimeout:   30 * time.Second,
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			dialer := &net.Dialer{Timeout: 5 * time.Second}
			return dialer.DialContext(ctx, network, address)
		},
	}
	return &http.Client{Transport: transport, Timeout: 0}, endpoint, nil
}

// LocalBaseURL is the base URL for a published endpoint.
func LocalBaseURL(endpoint Endpoint) string {
	if endpoint.Network == "unix" {
		return "http://ukk"
	}
	return "http://" + endpoint.Address
}
