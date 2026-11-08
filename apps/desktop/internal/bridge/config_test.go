package bridge

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestConfigRoundTripAndPermissions(t *testing.T) {
	dir := t.TempDir()
	config, err := LoadConfig(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(config.HostID) != 32 {
		t.Fatalf("host id = %q", config.HostID)
	}
	config.RelayURL = "wss://relay.example"
	config.TURN = TURN{URLs: []string{"turn:turn.example:3478"}, Username: "u", Credential: "c"}
	if err := SaveConfig(dir, config); err != nil {
		t.Fatal(err)
	}
	reloaded, err := LoadConfig(dir)
	if err != nil {
		t.Fatal(err)
	}
	if reloaded.RelayURL != config.RelayURL || reloaded.TURN.Credential != "c" {
		t.Fatalf("reloaded = %+v", reloaded)
	}
	info, err := os.Stat(filepath.Join(dir, "bridge.json"))
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		t.Fatalf("mode = %v", info.Mode())
	}
}

func TestConfigRejectsPlainRelayAndUnknownFields(t *testing.T) {
	dir := t.TempDir()
	config, _ := LoadConfig(dir)
	config.RelayURL = "http://relay.example"
	if err := SaveConfig(dir, config); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadConfig(dir); err == nil {
		t.Fatal("plain relay URL accepted")
	}
	if err := os.WriteFile(filepath.Join(dir, "bridge.json"), []byte(`{"host_id":"x","surprise":1}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadConfig(dir); err == nil || !strings.Contains(err.Error(), "unknown") {
		t.Fatalf("unknown field error = %v", err)
	}
}

func TestEndpointPublishAndStale(t *testing.T) {
	dir := t.TempDir()
	endpoint := Endpoint{Network: "unix", Address: "/tmp/x.sock", Token: "/tmp/token", PID: os.Getpid(), Version: Version, Started: time.Now().Add(-48 * time.Hour).UnixMilli()}
	if err := SaveEndpoint(dir, endpoint); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadEndpoint(dir, time.Hour); err == nil {
		t.Fatal("stale endpoint accepted")
	}
	endpoint.Started = time.Now().UnixMilli()
	if err := SaveEndpoint(dir, endpoint); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadEndpoint(dir, time.Hour); err != nil {
		t.Fatal(err)
	}
	ClearEndpoint(dir)
	if _, err := LoadEndpoint(dir, 0); err == nil {
		t.Fatal("cleared endpoint still readable")
	}
}

func TestListenIsPrivate(t *testing.T) {
	dir := t.TempDir()
	listener, endpoint, err := Listen(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	if endpoint.Version != Version || endpoint.Address == "" {
		t.Fatalf("endpoint = %+v", endpoint)
	}
	if endpoint.Network == "unix" {
		info, err := os.Stat(endpoint.Address)
		if err != nil {
			t.Fatal(err)
		}
		if info.Mode().Perm() != 0o600 {
			t.Fatalf("socket mode = %v", info.Mode())
		}
		if _, _, err := Listen(dir); err == nil {
			t.Fatal("second listener accepted on the same endpoint")
		}
	}
}

func TestLocalTokenPersistent(t *testing.T) {
	dir := t.TempDir()
	first, err := LocalToken(dir)
	if err != nil {
		t.Fatal(err)
	}
	second, err := LocalToken(dir)
	if err != nil {
		t.Fatal(err)
	}
	if first != second || len(first) < 43 {
		t.Fatalf("tokens = %q %q", first, second)
	}
}
