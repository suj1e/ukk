package extensions

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func jsPluginDir(t *testing.T, source string, trust string) string {
	t.Helper()
	dir := testDir(t)
	doc := helperManifestDoc("dev.ukk.test.js", "plugin.js", CommandScopeGlobal, nil, nil)
	doc["trust"] = trust
	writeManifestFile(t, dir, doc)
	if err := os.WriteFile(filepath.Join(dir, "plugin.js"), []byte(source), 0o600); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestJavaScriptPluginInvokesWithoutAmbientAuthority(t *testing.T) {
	dir := jsPluginDir(t, `
export function invoke(request) {
  return {
    command: request.command,
    args: request.args,
    processType: typeof process,
    fetchType: typeof fetch,
  };
}
`, TrustFirstParty)
	supervisor := NewSupervisor(Config{Root: testDir(t), CallTimeout: 2 * time.Second})
	defer supervisor.Shutdown()
	if _, err := supervisor.InstallDir(dir, InstallOptions{}); err != nil {
		t.Fatalf("install: %v", err)
	}
	if err := supervisor.Enable(context.Background(), "dev.ukk.test.js"); err != nil {
		t.Fatalf("enable: %v", err)
	}
	result, err := supervisor.Invoke(context.Background(), InvokeRequest{
		Plugin: "dev.ukk.test.js", Command: "test.echo",
		Args: json.RawMessage(`{"n":2}`),
	})
	if err != nil {
		t.Fatalf("invoke: %v", err)
	}
	var value struct {
		Command     string          `json:"command"`
		Args        json.RawMessage `json:"args"`
		ProcessType string          `json:"processType"`
		FetchType   string          `json:"fetchType"`
	}
	if err := json.Unmarshal(result.Value, &value); err != nil {
		t.Fatalf("result %s: %v", result.Value, err)
	}
	if value.Command != "test.echo" || string(value.Args) != `{"n":2}` {
		t.Fatalf("value = %+v", value)
	}
	if value.ProcessType != "undefined" || value.FetchType != "undefined" {
		t.Fatalf("ambient authority exposed: %+v", value)
	}
}

func TestJavaScriptPluginRejectsThirdPartyTrust(t *testing.T) {
	dir := jsPluginDir(t, `export function invoke() { return null; }`, TrustThirdParty)
	if _, err := LoadManifest(filepath.Join(dir, "plugin.json")); err == nil {
		t.Fatal("third-party JavaScript manifest was accepted")
	}
}

func TestJavaScriptPluginRequiresInvokeExport(t *testing.T) {
	dir := jsPluginDir(t, `export function somethingElse() { return 1; }`, TrustFirstParty)
	supervisor := NewSupervisor(Config{Root: testDir(t)})
	defer supervisor.Shutdown()
	if _, err := supervisor.InstallDir(dir, InstallOptions{}); err != nil {
		t.Fatal(err)
	}
	if err := supervisor.Enable(context.Background(), "dev.ukk.test.js"); err == nil {
		t.Fatal("enable without invoke export succeeded")
	}
}

func TestJavaScriptPluginCannotImportOrEscapeDeadline(t *testing.T) {
	dir := jsPluginDir(t, `import fs from "node:fs"; export function invoke() { return 1; }`, TrustFirstParty)
	supervisor := NewSupervisor(Config{Root: testDir(t), CallTimeout: 2 * time.Second})
	defer supervisor.Shutdown()
	if _, err := supervisor.InstallDir(dir, InstallOptions{}); err != nil {
		t.Fatal(err)
	}
	if err := supervisor.Enable(context.Background(), "dev.ukk.test.js"); err != nil {
		t.Fatal(err)
	}
	if _, err := supervisor.Invoke(context.Background(), InvokeRequest{Plugin: "dev.ukk.test.js", Command: "test.echo"}); err == nil || !strings.Contains(err.Error(), "load JavaScript plugin") {
		t.Fatalf("import error = %v", err)
	}

	dir = jsPluginDir(t, `export function invoke() { for (;;) {} }`, TrustFirstParty)
	supervisor2 := NewSupervisor(Config{Root: testDir(t), CallTimeout: 100 * time.Millisecond})
	defer supervisor2.Shutdown()
	if _, err := supervisor2.InstallDir(dir, InstallOptions{}); err != nil {
		t.Fatal(err)
	}
	if err := supervisor2.Enable(context.Background(), "dev.ukk.test.js"); err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	if _, err := supervisor2.Invoke(context.Background(), InvokeRequest{Plugin: "dev.ukk.test.js", Command: "test.echo"}); CodeOf(err) != CodeHelperTimeout {
		t.Fatalf("timeout error = %v", err)
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Fatalf("timeout took %s", elapsed)
	}
}

func TestHelloJSExample(t *testing.T) {
	dir := filepath.Join("..", "..", "examples", "plugins", "hello-js")
	supervisor := NewSupervisor(Config{Root: testDir(t)})
	defer supervisor.Shutdown()
	if _, err := supervisor.InstallDir(dir, InstallOptions{}); err != nil {
		t.Fatalf("install example: %v", err)
	}
	if err := supervisor.Enable(context.Background(), "dev.ukk.example.hello-js"); err != nil {
		t.Fatalf("enable example: %v", err)
	}
	result, err := supervisor.Invoke(context.Background(), InvokeRequest{
		Plugin: "dev.ukk.example.hello-js", Command: "hello.echo",
	})
	if err != nil {
		t.Fatalf("invoke example: %v", err)
	}
	if !strings.Contains(string(result.Value), "hello from moejs") {
		t.Fatalf("example result = %s", result.Value)
	}
}
