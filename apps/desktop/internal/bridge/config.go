// Package bridge is the local background service: it owns Agent children,
// remote authorization, relay connection and Remote Screen. The Desktop talks
// to it over a user-private local endpoint; the same command dispatcher serves
// paired devices over the relay.
package bridge

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

// Version is the bridge protocol version this build speaks.
const Version = 1

// TURN is an optional relayed media server for Remote Screen.
type TURN struct {
	URLs       []string `json:"urls,omitempty"`
	Username   string   `json:"username,omitempty"`
	Credential string   `json:"credential,omitempty"`
}

// Config is the persistent bridge configuration.
type Config struct {
	HostID        string   `json:"host_id"`
	RelayURL      string   `json:"relay_url,omitempty"`
	OperatorToken string   `json:"operator_token,omitempty"`
	TURN          TURN     `json:"turn,omitempty"`
	STUNURLs      []string `json:"stun_urls,omitempty"`
}

// DataDir resolves the per-user ukk data directory. UKK_DATA_DIR overrides it,
// which tests and portable installs use.
func DataDir() (string, error) {
	if override := strings.TrimSpace(os.Getenv("UKK_DATA_DIR")); override != "" {
		return override, os.MkdirAll(override, 0o700)
	}
	var base string
	switch runtime.GOOS {
	case "darwin":
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		base = filepath.Join(home, "Library", "Application Support")
	case "windows":
		base = os.Getenv("AppData")
		if base == "" {
			return "", errors.New("bridge: AppData is not set")
		}
	default:
		base = os.Getenv("XDG_CONFIG_HOME")
		if base == "" {
			home, err := os.UserHomeDir()
			if err != nil {
				return "", err
			}
			base = filepath.Join(home, ".config")
		}
	}
	dir := filepath.Join(base, "ukk")
	return dir, os.MkdirAll(dir, 0o700)
}

// LoadConfig reads bridge.json, creating a default document with a fresh host
// ID on first use.
func LoadConfig(dir string) (Config, error) {
	path := filepath.Join(dir, "bridge.json")
	raw, err := os.ReadFile(path)
	switch {
	case errors.Is(err, os.ErrNotExist):
		config := Config{HostID: newToken(16)}
		if err := SaveConfig(dir, config); err != nil {
			return Config{}, err
		}
		return config, nil
	case err != nil:
		return Config{}, fmt.Errorf("bridge: read config: %w", err)
	}
	var config Config
	decoder := json.NewDecoder(strings.NewReader(string(raw)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&config); err != nil {
		return Config{}, fmt.Errorf("bridge: decode config: %w", err)
	}
	if config.HostID == "" {
		return Config{}, errors.New("bridge: config has no host_id")
	}
	if strings.TrimSpace(config.RelayURL) != "" && !strings.HasPrefix(config.RelayURL, "wss://") && !strings.HasPrefix(config.RelayURL, "ws://127.0.0.1") {
		return Config{}, errors.New("bridge: relay_url must use wss:// (or ws://127.0.0.1 for local tests)")
	}
	return config, nil
}

// SaveConfig writes bridge.json atomically with owner-only permissions.
func SaveConfig(dir string, config Config) error {
	raw, err := json.MarshalIndent(config, "", "  ")
	if err != nil {
		return err
	}
	return writeAtomic(filepath.Join(dir, "bridge.json"), append(raw, '\n'))
}

// LocalToken returns the local API bearer token, creating it on first use.
func LocalToken(dir string) (string, error) {
	path := filepath.Join(dir, "bridge.token")
	raw, err := os.ReadFile(path)
	if err == nil {
		token := strings.TrimSpace(string(raw))
		if len(token) >= 43 {
			return token, nil
		}
		return "", errors.New("bridge: local token is truncated")
	}
	if !errors.Is(err, os.ErrNotExist) {
		return "", err
	}
	token := newToken(32)
	if err := writeAtomic(path, []byte(token+"\n")); err != nil {
		return "", err
	}
	return token, nil
}

func writeAtomic(path string, data []byte) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, ".ukk-*.tmp")
	if err != nil {
		return err
	}
	name := tmp.Name()
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		os.Remove(name)
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		os.Remove(name)
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		os.Remove(name)
		return err
	}
	if err := tmp.Close(); err != nil {
		os.Remove(name)
		return err
	}
	return os.Rename(name, path)
}

func newToken(bytes int) string {
	buf := make([]byte, bytes)
	if _, err := rand.Read(buf); err != nil {
		panic("bridge: crypto/rand unavailable: " + err.Error())
	}
	return hex.EncodeToString(buf)
}

// Endpoint describes how a local client reaches the running bridge.
type Endpoint struct {
	Network string `json:"network"`
	Address string `json:"address"`
	Token   string `json:"token_path"`
	PID     int    `json:"pid"`
	Version int    `json:"version"`
	Started int64  `json:"started_ms"`
}

// SaveEndpoint publishes the live endpoint for Desktop discovery.
func SaveEndpoint(dir string, endpoint Endpoint) error {
	raw, err := json.MarshalIndent(endpoint, "", "  ")
	if err != nil {
		return err
	}
	return writeAtomic(filepath.Join(dir, "bridge.endpoint.json"), append(raw, '\n'))
}

// LoadEndpoint reads a published endpoint, rejecting a stale one.
func LoadEndpoint(dir string, maxAge time.Duration) (Endpoint, error) {
	raw, err := os.ReadFile(filepath.Join(dir, "bridge.endpoint.json"))
	if err != nil {
		return Endpoint{}, err
	}
	var endpoint Endpoint
	if err := json.Unmarshal(raw, &endpoint); err != nil {
		return Endpoint{}, fmt.Errorf("bridge: decode endpoint: %w", err)
	}
	if endpoint.Network == "" || endpoint.Address == "" || endpoint.Token == "" {
		return Endpoint{}, errors.New("bridge: incomplete endpoint")
	}
	if maxAge > 0 && time.Since(time.UnixMilli(endpoint.Started)) > maxAge {
		return Endpoint{}, errors.New("bridge: endpoint is stale")
	}
	return endpoint, nil
}

// ClearEndpoint removes discovery state on a clean shutdown.
func ClearEndpoint(dir string) {
	_ = os.Remove(filepath.Join(dir, "bridge.endpoint.json"))
}
