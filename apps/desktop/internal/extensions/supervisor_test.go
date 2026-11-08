package extensions

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

const supervisorTestID = "dev.ukk.test.helper"

func helperProgramName() string {
	if runtime.GOOS == "windows" {
		return "ukk-test-helper.exe"
	}
	return "ukk-test-helper"
}

func helperManifestDoc(id, entrypoint, scope string, required, requires []string) map[string]any {
	declared := make([]any, 0, len(required))
	for _, capability := range required {
		declared = append(declared, capability)
	}
	needed := make([]any, 0, len(requires))
	for _, capability := range requires {
		needed = append(needed, capability)
	}
	return map[string]any{
		"manifest_version": 1,
		"id":               id,
		"name":             "Helper test extension",
		"version":          "1.2.3",
		"api":              ">=0.1,<0.2",
		"trust":            "first-party",
		"entrypoint":       entrypoint,
		"capabilities": map[string]any{
			"required": declared,
			"optional": []any{},
		},
		"contributions": map[string]any{
			"commands": []any{
				map[string]any{
					"id":       "test.echo",
					"title":    "Echo",
					"scope":    scope,
					"requires": needed,
				},
			},
			"views":  []any{},
			"events": []any{},
		},
	}
}

func builtinManifestDoc(id string, required, requires []string) map[string]any {
	doc := helperManifestDoc(id, "", CommandScopeGlobal, required, requires)
	doc["trust"] = TrustBuiltin
	doc["name"] = "Builtin test extension"
	return doc
}

func writeManifestFile(t *testing.T, dir string, doc map[string]any) {
	t.Helper()
	data, err := json.Marshal(doc)
	if err != nil {
		t.Fatalf("marshal manifest: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "plugin.json"), data, 0o600); err != nil {
		t.Fatalf("write manifest: %v", err)
	}
}

func copyTestExecutable(t *testing.T, dst string) {
	t.Helper()
	src, err := os.Executable()
	if err != nil {
		t.Fatalf("locate test executable: %v", err)
	}
	data, err := os.ReadFile(src)
	if err != nil {
		t.Fatalf("read test executable: %v", err)
	}
	if err := os.WriteFile(dst, data, 0o755); err != nil {
		t.Fatalf("write helper executable: %v", err)
	}
}

func helperInstallOptions(mode string) InstallOptions {
	return InstallOptions{
		Env: map[string]string{
			"GO_EXTENSIONS_HELPER":      "1",
			"GO_EXTENSIONS_HELPER_MODE": mode,
		},
		Args: []string{"-test.run=^TestHelperProcess$"},
	}
}

// installHelperPlugin writes a plugin directory containing a copy of the test
// binary named like the manifest entrypoint, then installs it.
func installHelperPlugin(t *testing.T, sup *Supervisor, mode, scope string, required, requires []string) (string, string) {
	t.Helper()
	dir := testDir(t)
	name := helperProgramName()
	copyTestExecutable(t, filepath.Join(dir, name))
	writeManifestFile(t, dir, helperManifestDoc(supervisorTestID, name, scope, required, requires))
	manifest, err := sup.InstallDir(dir, helperInstallOptions(mode))
	if err != nil {
		t.Fatalf("InstallDir() error = %v", err)
	}
	if manifest.ID != supervisorTestID {
		t.Fatalf("installed id = %q", manifest.ID)
	}
	return dir, supervisorTestID
}

func lastRecord(t *testing.T, audit *Audit) AuditRecord {
	t.Helper()
	records := audit.Records()
	if len(records) == 0 {
		t.Fatal("audit is empty")
	}
	return records[len(records)-1]
}

func TestHelperInvokeSuccess(t *testing.T) {
	sup := NewSupervisor(Config{})
	t.Cleanup(sup.Shutdown)
	dir, id := installHelperPlugin(t, sup, "ok", CommandScopeGlobal, nil, nil)
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}
	status, err := sup.Status(id)
	if err != nil {
		t.Fatalf("Status() error = %v", err)
	}
	if status.State != StateReady {
		t.Fatalf("state = %q, want %q", status.State, StateReady)
	}
	if status.PID == 0 {
		t.Fatal("ready helper has no pid")
	}

	t.Setenv("EXT_LEAK", "must-not-reach-helper")
	result, err := sup.Invoke(context.Background(), InvokeRequest{
		Plugin:  id,
		Command: "test.echo",
		Args:    json.RawMessage(`{"note":"arg-marker-123"}`),
	})
	if err != nil {
		t.Fatalf("Invoke() error = %v", err)
	}
	var value map[string]any
	if err := json.Unmarshal(result.Value, &value); err != nil {
		t.Fatalf("decode result: %v", err)
	}
	if value["pong"] != true || value["command"] != "test.echo" {
		t.Fatalf("result value = %v", value)
	}
	if value["mode"] != "ok" {
		t.Fatalf("declared env mode = %v, want ok", value["mode"])
	}
	if leak, ok := value["leak"]; ok && leak != "" {
		t.Fatalf("undeclared env leaked into helper: %v", leak)
	}
	record := lastRecord(t, sup.Audit())
	if record.Action != ActionInvoke || record.Outcome != OutcomeSuccess || record.Plugin != id {
		t.Fatalf("audit record = %+v", record)
	}
	if !strings.Contains(record.Detail, "command=test.echo") {
		t.Fatalf("audit detail = %q", record.Detail)
	}
	if strings.Contains(record.Detail, "arg-marker-123") {
		t.Fatalf("audit detail leaked command arguments: %q", record.Detail)
	}

	bigArgs := json.RawMessage(`"` + strings.Repeat("a", MaxFrameBytes) + `"`)
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo", Args: bigArgs}); !IsCode(err, CodeInvalidArgs) {
		t.Fatalf("Invoke(oversized args) error = %v, want %s", err, CodeInvalidArgs)
	}
	status, err = sup.Status(id)
	if err != nil || status.State != StateReady {
		t.Fatalf("oversized arguments failed the plugin: status=%+v err=%v", status, err)
	}

	_ = dir
}

func TestInvokeDeniedWithoutGrantIsAudited(t *testing.T) {
	sup := NewSupervisor(Config{})
	t.Cleanup(sup.Shutdown)
	_, id := installHelperPlugin(t, sup, "ok", CommandScopeSession, []string{CapSessionObserve}, []string{CapSessionObserve})
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}

	_, err := sup.Invoke(context.Background(), InvokeRequest{
		Plugin:    id,
		Command:   "test.echo",
		SessionID: "sess-1",
	})
	if !IsCode(err, CodeCapabilityDenied) {
		t.Fatalf("Invoke() error = %v, want %s", err, CodeCapabilityDenied)
	}
	record := lastRecord(t, sup.Audit())
	if record.Action != ActionInvoke || record.Outcome != OutcomeDenied {
		t.Fatalf("audit record = %+v, want a denied invoke", record)
	}
	if record.Capability != CapSessionObserve || record.Scope != SessionScope("sess-1") {
		t.Fatalf("audit record = %+v", record)
	}
	if strings.Contains(record.Detail, "payload") {
		t.Fatalf("audit detail leaks payload wording: %q", record.Detail)
	}

	if _, err := sup.Grants().Grant(id, CapSessionObserve, ScopeHost, 0); err != nil {
		t.Fatalf("Grant() error = %v", err)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{
		Plugin:    id,
		Command:   "test.echo",
		SessionID: "sess-1",
	}); err != nil {
		t.Fatalf("Invoke() after grant error = %v", err)
	}
	if record := lastRecord(t, sup.Audit()); record.Outcome != OutcomeSuccess {
		t.Fatalf("audit after grant = %+v", record)
	}

	// Session-scoped commands require a session id even with a grant.
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); !IsCode(err, CodeMissingSession) {
		t.Fatalf("Invoke(no session) error = %v, want %s", err, CodeMissingSession)
	}
}

func TestInvokeTimeoutKillsHelper(t *testing.T) {
	sup := NewSupervisor(Config{})
	t.Cleanup(sup.Shutdown)
	_, id := installHelperPlugin(t, sup, "hang", CommandScopeGlobal, nil, nil)
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}
	// Spawn and handshake use the default deadline; the invoke deadline is
	// shortened here so the test does not wait seconds for a hung helper.
	sup.callTimeout = 50 * time.Millisecond
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); !IsCode(err, CodeHelperTimeout) {
		t.Fatalf("Invoke() error = %v, want %s", err, CodeHelperTimeout)
	}
	status, err := sup.Status(id)
	if err != nil {
		t.Fatalf("Status() error = %v", err)
	}
	if status.State != StateFailed {
		t.Fatalf("state = %q, want %q", status.State, StateFailed)
	}
	if status.PID != 0 {
		t.Fatalf("timed-out helper still has pid %d", status.PID)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); !IsCode(err, CodeNotEnabled) {
		t.Fatalf("Invoke() after timeout error = %v, want %s", err, CodeNotEnabled)
	}
	record := lastRecord(t, sup.Audit())
	if record.Outcome != OutcomeDenied {
		t.Fatalf("audit record after timeout = %+v", record)
	}
}

func TestOversizedHelperFrameKillsChild(t *testing.T) {
	sup := NewSupervisor(Config{})
	t.Cleanup(sup.Shutdown)
	_, id := installHelperPlugin(t, sup, "oversize", CommandScopeGlobal, nil, nil)
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); !IsCode(err, CodeFrameTooLarge) {
		t.Fatalf("Invoke() error = %v, want %s", err, CodeFrameTooLarge)
	}
	status, err := sup.Status(id)
	if err != nil {
		t.Fatalf("Status() error = %v", err)
	}
	if status.State != StateFailed || status.PID != 0 {
		t.Fatalf("status after oversized frame = %+v", status)
	}
	records := sup.Audit().Records()
	found := false
	for _, record := range records {
		if record.Action == ActionInvoke && record.Outcome == OutcomeError && strings.Contains(record.Detail, CodeFrameTooLarge) {
			found = true
		}
	}
	if !found {
		t.Fatalf("no audit record for oversized frame: %+v", records)
	}
}

func TestDisableStopsHelperAndInvokeFails(t *testing.T) {
	sup := NewSupervisor(Config{})
	t.Cleanup(sup.Shutdown)
	_, id := installHelperPlugin(t, sup, "ok", CommandScopeGlobal, nil, nil)
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); err != nil {
		t.Fatalf("Invoke() error = %v", err)
	}
	if err := sup.Disable(id); err != nil {
		t.Fatalf("Disable() error = %v", err)
	}
	status, err := sup.Status(id)
	if err != nil {
		t.Fatalf("Status() error = %v", err)
	}
	if status.State != StateDisabled || status.PID != 0 {
		t.Fatalf("status after disable = %+v", status)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); !IsCode(err, CodeNotEnabled) {
		t.Fatalf("Invoke() after disable error = %v, want %s", err, CodeNotEnabled)
	}
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("re-Enable() error = %v", err)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); err != nil {
		t.Fatalf("Invoke() after re-enable error = %v", err)
	}
}

func TestUninstallRemovesPluginAndGrants(t *testing.T) {
	sup := NewSupervisor(Config{})
	t.Cleanup(sup.Shutdown)
	dir, id := installHelperPlugin(t, sup, "ok", CommandScopeGlobal, nil, nil)
	if _, err := sup.Grants().Grant(id, CapWorkspaceRead, ScopeHost, 0); err != nil {
		t.Fatalf("Grant() error = %v", err)
	}
	if err := sup.Enable(context.Background(), id); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}
	if err := sup.Uninstall(id); err != nil {
		t.Fatalf("Uninstall() error = %v", err)
	}
	if _, err := sup.Status(id); !IsCode(err, CodeUnknownPlugin) {
		t.Fatalf("Status() after uninstall error = %v, want %s", err, CodeUnknownPlugin)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("plugin directory still exists: %v", err)
	}
	if sup.Grants().Authorized(id, CapWorkspaceRead, ScopeHost, time.Now()) {
		t.Fatal("uninstall left a grant active")
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: id, Command: "test.echo"}); !IsCode(err, CodeUnknownPlugin) {
		t.Fatalf("Invoke() after uninstall error = %v, want %s", err, CodeUnknownPlugin)
	}
}

func TestInstallRejectsEntrypointMismatch(t *testing.T) {
	sup := NewSupervisor(Config{})
	dir := testDir(t)
	copyTestExecutable(t, filepath.Join(dir, helperProgramName()))
	writeManifestFile(t, dir, helperManifestDoc(supervisorTestID, "other-helper", CommandScopeGlobal, nil, nil))
	if _, err := sup.InstallDir(dir, InstallOptions{}); !IsCode(err, CodeInvalidHelper) {
		t.Fatalf("InstallDir() error = %v, want %s", err, CodeInvalidHelper)
	}
}

type builtinEcho struct {
	calls int
	last  InvokeRequest
}

func (b *builtinEcho) Invoke(_ context.Context, request InvokeRequest) (json.RawMessage, error) {
	b.calls++
	b.last = request
	return json.RawMessage(`{"builtin":true}`), nil
}

func TestBuiltinPath(t *testing.T) {
	impl := &builtinEcho{}
	sup := NewSupervisor(Config{Builtins: map[string]InProcess{"dev.ukk.test.builtin": impl}})
	t.Cleanup(sup.Shutdown)
	doc := builtinManifestDoc("dev.ukk.test.builtin", []string{CapAgentSend}, []string{CapAgentSend})
	data, err := json.Marshal(doc)
	if err != nil {
		t.Fatal(err)
	}
	manifest, err := ValidateManifest(data)
	if err != nil {
		t.Fatalf("ValidateManifest() error = %v", err)
	}
	if err := sup.Install(manifest, "", InstallOptions{}); err != nil {
		t.Fatalf("Install() error = %v", err)
	}
	if err := sup.Enable(context.Background(), "dev.ukk.test.builtin"); err != nil {
		t.Fatalf("Enable() error = %v", err)
	}
	status, err := sup.Status("dev.ukk.test.builtin")
	if err != nil || status.State != StateReady || status.PID != 0 {
		t.Fatalf("builtin status = %+v, %v", status, err)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: "dev.ukk.test.builtin", Command: "test.echo"}); !IsCode(err, CodeCapabilityDenied) {
		t.Fatalf("Invoke() error = %v, want %s", err, CodeCapabilityDenied)
	}
	if impl.calls != 0 {
		t.Fatalf("builtin was called before authorization: %d calls", impl.calls)
	}
	if _, err := sup.Grants().Grant("dev.ukk.test.builtin", CapAgentSend, ScopeHost, 0); err != nil {
		t.Fatalf("Grant() error = %v", err)
	}
	result, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: "dev.ukk.test.builtin", Command: "test.echo"})
	if err != nil {
		t.Fatalf("Invoke() after grant error = %v", err)
	}
	if string(result.Value) != `{"builtin":true}` {
		t.Fatalf("builtin result = %s", result.Value)
	}
	if impl.calls != 1 || impl.last.Command != "test.echo" {
		t.Fatalf("builtin calls = %d, last = %+v", impl.calls, impl.last)
	}
	if err := sup.Disable("dev.ukk.test.builtin"); err != nil {
		t.Fatalf("Disable() error = %v", err)
	}
	if _, err := sup.Invoke(context.Background(), InvokeRequest{Plugin: "dev.ukk.test.builtin", Command: "test.echo"}); !IsCode(err, CodeNotEnabled) {
		t.Fatalf("Invoke() after disable error = %v, want %s", err, CodeNotEnabled)
	}
}

func TestBuiltinRequiresRegisteredImplementation(t *testing.T) {
	sup := NewSupervisor(Config{})
	data, err := json.Marshal(builtinManifestDoc("dev.ukk.test.builtin", nil, nil))
	if err != nil {
		t.Fatal(err)
	}
	manifest, err := ValidateManifest(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := sup.Install(manifest, "", InstallOptions{}); !IsCode(err, CodeInvalidHelper) {
		t.Fatalf("Install() error = %v, want %s", err, CodeInvalidHelper)
	}
}

func TestInstallRejectsReservedEnvironment(t *testing.T) {
	sup := NewSupervisor(Config{})
	dir, _ := installHelperPlugin(t, sup, "ok", CommandScopeGlobal, nil, nil)
	_ = dir
	data, err := json.Marshal(helperManifestDoc("dev.ukk.test.second", helperProgramName(), CommandScopeGlobal, nil, nil))
	if err != nil {
		t.Fatal(err)
	}
	manifest, err := ValidateManifest(data)
	if err != nil {
		t.Fatal(err)
	}
	err = sup.Install(manifest, filepath.Join(dir, helperProgramName()), InstallOptions{
		Env: map[string]string{"PATH": "/tmp"},
	})
	if !IsCode(err, CodeInvalidArgs) {
		t.Fatalf("Install() error = %v, want %s", err, CodeInvalidArgs)
	}
}
