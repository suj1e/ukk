package extensions

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func baseManifest() map[string]any {
	return map[string]any{
		"manifest_version": 1,
		"id":               "dev.ukk.test.sample",
		"name":             "Sample Extension",
		"version":          "0.1.0",
		"api":              ">=0.1,<0.2",
		"trust":            "first-party",
		"entrypoint":       "ukk-test-helper",
		"capabilities": map[string]any{
			"required": []any{"session.observe"},
			"optional": []any{"network.connect(api.example.com)"},
		},
		"contributions": map[string]any{
			"commands": []any{
				map[string]any{
					"id":          "sample.ping",
					"title":       "Sample ping",
					"description": "Ping the session",
					"scope":       "session",
					"requires":    []any{"session.observe"},
				},
			},
			"views": []any{
				map[string]any{
					"id":        "sample.tab",
					"slot":      "session.tab",
					"title":     "Sample",
					"command":   "sample.ping",
					"platforms": []any{"desktop", "ios"},
				},
			},
			"events": []any{
				map[string]any{
					"id":    "sample.events",
					"kinds": []any{"session.updated"},
				},
			},
		},
	}
}

func baseBuiltinManifest() map[string]any {
	m := baseManifest()
	delete(m, "entrypoint")
	m["trust"] = "builtin"
	return m
}

func commandsOf(m map[string]any) []any {
	return m["contributions"].(map[string]any)["commands"].([]any)
}

func firstCommand(m map[string]any) map[string]any {
	return commandsOf(m)[0].(map[string]any)
}

func firstView(m map[string]any) map[string]any {
	return m["contributions"].(map[string]any)["views"].([]any)[0].(map[string]any)
}

func firstEvent(m map[string]any) map[string]any {
	return m["contributions"].(map[string]any)["events"].([]any)[0].(map[string]any)
}

func capabilitiesOf(m map[string]any) map[string]any {
	return m["capabilities"].(map[string]any)
}

func TestValidateManifest(t *testing.T) {
	longID := strings.Repeat("a", 65)
	longTitle := strings.Repeat("t", 81)

	tests := []struct {
		name     string
		base     func() map[string]any
		mutate   func(map[string]any)
		wantCode string
	}{
		{name: "valid helper manifest", base: baseManifest},
		{name: "valid builtin manifest", base: baseBuiltinManifest},
		{
			name: "valid wildcard network domain",
			mutate: func(m map[string]any) {
				capabilitiesOf(m)["optional"] = []any{"network.connect(*.example.com)"}
			},
		},
		{
			name:   "valid multiline api range",
			mutate: func(m map[string]any) { m["api"] = ">=0.1, <0.2, >0.0.9" },
		},
		{
			name: "valid overlay view",
			mutate: func(m map[string]any) {
				firstView(m)["slot"] = SlotSessionOverlay
				firstView(m)["overlay"] = map[string]any{"anchor": "top-right", "max_width": 320, "max_height": 240}
			},
		},
		{
			name:     "unknown top level field",
			mutate:   func(m map[string]any) { m["colour"] = "red" },
			wantCode: CodeUnknownField,
		},
		{
			name:     "unknown nested field",
			mutate:   func(m map[string]any) { capabilitiesOf(m)["extra"] = true },
			wantCode: CodeUnknownField,
		},
		{
			name:     "unknown view field",
			mutate:   func(m map[string]any) { firstView(m)["colour"] = "red" },
			wantCode: CodeUnknownField,
		},
		{
			name:     "unsupported manifest version",
			mutate:   func(m map[string]any) { m["manifest_version"] = 2 },
			wantCode: CodeUnsupportedManifestVersion,
		},
		{
			name:     "single component plugin id",
			mutate:   func(m map[string]any) { m["id"] = "sample" },
			wantCode: CodeInvalidID,
		},
		{
			name:     "uppercase plugin id",
			mutate:   func(m map[string]any) { m["id"] = "Dev.Ukk.Sample" },
			wantCode: CodeInvalidID,
		},
		{
			name:     "oversized plugin id",
			mutate:   func(m map[string]any) { m["id"] = "dev.ukk." + strings.Repeat("a", 60) },
			wantCode: CodeInvalidID,
		},
		{
			name:     "empty name",
			mutate:   func(m map[string]any) { m["name"] = "" },
			wantCode: CodeInvalidName,
		},
		{
			name:     "oversized title",
			mutate:   func(m map[string]any) { firstCommand(m)["title"] = longTitle },
			wantCode: CodeInvalidName,
		},
		{
			name:     "prerelease version",
			mutate:   func(m map[string]any) { m["version"] = "0.1.0-beta" },
			wantCode: CodeInvalidVersion,
		},
		{
			name:     "two component version",
			mutate:   func(m map[string]any) { m["version"] = "0.1" },
			wantCode: CodeInvalidVersion,
		},
		{
			name:     "malformed api range",
			mutate:   func(m map[string]any) { m["api"] = "0.1" },
			wantCode: CodeInvalidAPIRange,
		},
		{
			name:     "non matching api range",
			mutate:   func(m map[string]any) { m["api"] = ">=0.2,<0.3" },
			wantCode: CodeInvalidAPIRange,
		},
		{
			name:     "empty api bound",
			mutate:   func(m map[string]any) { m["api"] = ">=0.1,,<0.2" },
			wantCode: CodeInvalidAPIRange,
		},
		{
			name:     "unknown trust",
			mutate:   func(m map[string]any) { m["trust"] = "sandboxed-wasm" },
			wantCode: CodeUnknownTrust,
		},
		{
			name:     "entrypoint with slash",
			mutate:   func(m map[string]any) { m["entrypoint"] = "bin/helper" },
			wantCode: CodeInvalidEntrypoint,
		},
		{
			name:     "entrypoint with backslash",
			mutate:   func(m map[string]any) { m["entrypoint"] = `bin\helper` },
			wantCode: CodeInvalidEntrypoint,
		},
		{
			name:     "entrypoint with dot dot",
			mutate:   func(m map[string]any) { m["entrypoint"] = ".." },
			wantCode: CodeInvalidEntrypoint,
		},
		{
			name:     "helper without entrypoint",
			mutate:   func(m map[string]any) { delete(m, "entrypoint") },
			wantCode: CodeInvalidEntrypoint,
		},
		{
			name:     "unknown capability",
			mutate:   func(m map[string]any) { capabilitiesOf(m)["optional"] = []any{"filesystem.exec"} },
			wantCode: CodeUnknownCapability,
		},
		{
			name:     "bare network capability",
			mutate:   func(m map[string]any) { capabilitiesOf(m)["optional"] = []any{"network.connect"} },
			wantCode: CodeUnknownCapability,
		},
		{
			name:     "network wildcard without domain",
			mutate:   func(m map[string]any) { capabilitiesOf(m)["optional"] = []any{"network.connect(*)"} },
			wantCode: CodeUnknownCapability,
		},
		{
			name:     "duplicate capability",
			mutate:   func(m map[string]any) { capabilitiesOf(m)["optional"] = []any{"session.observe"} },
			wantCode: CodeDuplicateID,
		},
		{
			name: "duplicate command id",
			mutate: func(m map[string]any) {
				cmds := commandsOf(m)
				m["contributions"].(map[string]any)["commands"] = append(cmds, cmds[0])
			},
			wantCode: CodeDuplicateID,
		},
		{
			name: "duplicate view id",
			mutate: func(m map[string]any) {
				views := m["contributions"].(map[string]any)["views"].([]any)
				m["contributions"].(map[string]any)["views"] = append(views, views[0])
			},
			wantCode: CodeDuplicateID,
		},
		{
			name: "duplicate event id",
			mutate: func(m map[string]any) {
				evs := m["contributions"].(map[string]any)["events"].([]any)
				m["contributions"].(map[string]any)["events"] = append(evs, evs[0])
			},
			wantCode: CodeDuplicateID,
		},
		{
			name:     "requires undeclared capability",
			mutate:   func(m map[string]any) { firstCommand(m)["requires"] = []any{"workspace.read"} },
			wantCode: CodeUndeclaredCapability,
		},
		{
			name:     "requires unknown capability",
			mutate:   func(m map[string]any) { firstCommand(m)["requires"] = []any{"system.root"} },
			wantCode: CodeUnknownCapability,
		},
		{
			name:     "unknown command scope",
			mutate:   func(m map[string]any) { firstCommand(m)["scope"] = "space" },
			wantCode: CodeUnknownScope,
		},
		{
			name:     "unknown slot",
			mutate:   func(m map[string]any) { firstView(m)["slot"] = "session.sidebar" },
			wantCode: CodeUnknownSlot,
		},
		{
			name:     "view binds undeclared command",
			mutate:   func(m map[string]any) { firstView(m)["command"] = "sample.missing" },
			wantCode: CodeUnknownCommand,
		},
		{
			name:     "unknown platform",
			mutate:   func(m map[string]any) { firstView(m)["platforms"] = []any{"android"} },
			wantCode: CodeUnknownPlatform,
		},
		{
			name:     "overlay missing for overlay slot",
			mutate:   func(m map[string]any) { firstView(m)["slot"] = SlotSessionOverlay },
			wantCode: CodeInvalidOverlay,
		},
		{
			name: "overlay forbidden on tab slot",
			mutate: func(m map[string]any) {
				firstView(m)["overlay"] = map[string]any{"anchor": "top-left", "max_width": 200, "max_height": 200}
			},
			wantCode: CodeInvalidOverlay,
		},
		{
			name: "overlay width out of bounds",
			mutate: func(m map[string]any) {
				firstView(m)["slot"] = SlotSessionOverlay
				firstView(m)["overlay"] = map[string]any{"anchor": "top-left", "max_width": 50, "max_height": 200}
			},
			wantCode: CodeInvalidOverlay,
		},
		{
			name: "overlay unknown anchor",
			mutate: func(m map[string]any) {
				firstView(m)["slot"] = SlotSessionOverlay
				firstView(m)["overlay"] = map[string]any{"anchor": "middle", "max_width": 200, "max_height": 200}
			},
			wantCode: CodeInvalidOverlay,
		},
		{
			name: "event without session.observe",
			mutate: func(m map[string]any) {
				capabilitiesOf(m)["required"] = []any{}
				capabilitiesOf(m)["optional"] = []any{}
				firstCommand(m)["requires"] = []any{}
			},
			wantCode: CodeUndeclaredCapability,
		},
		{
			name:     "unknown event kind",
			mutate:   func(m map[string]any) { firstEvent(m)["kinds"] = []any{"terminal.output"} },
			wantCode: CodeUnknownEventKind,
		},
		{
			name:     "empty event kinds",
			mutate:   func(m map[string]any) { firstEvent(m)["kinds"] = []any{} },
			wantCode: CodeUnknownEventKind,
		},
		{
			name:     "oversized command id",
			mutate:   func(m map[string]any) { firstCommand(m)["id"] = longID },
			wantCode: CodeInvalidID,
		},
		{
			name: "too many commands",
			mutate: func(m map[string]any) {
				cmds := make([]any, 0, MaxCommands+1)
				for i := 0; i <= MaxCommands; i++ {
					cmds = append(cmds, map[string]any{
						"id":       "sample.cmd" + strings.Repeat("x", i%4) + string(rune('a'+i%26)) + strings.Repeat("0", i/26),
						"title":    "Cmd",
						"scope":    "global",
						"requires": []any{},
					})
				}
				m["contributions"].(map[string]any)["commands"] = cmds
			},
			wantCode: CodeLimitExceeded,
		},
		{
			name: "too many capabilities",
			mutate: func(m map[string]any) {
				list := make([]any, 0, MaxCapabilitiesPerList+1)
				for i := 0; i <= MaxCapabilitiesPerList; i++ {
					list = append(list, "network.connect(host"+string(rune('a'+i%26))+".example.com)")
				}
				capabilitiesOf(m)["optional"] = list
			},
			wantCode: CodeLimitExceeded,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			base := baseManifest
			if tc.base != nil {
				base = tc.base
			}
			manifest := base()
			if tc.mutate != nil {
				tc.mutate(manifest)
			}
			data, err := json.Marshal(manifest)
			if err != nil {
				t.Fatalf("marshal fixture: %v", err)
			}
			validated, err := ValidateManifest(data)
			if tc.wantCode == "" {
				if err != nil {
					t.Fatalf("ValidateManifest() error = %v, want success", err)
				}
				if validated == nil {
					t.Fatal("ValidateManifest() returned nil manifest")
				}
				return
			}
			if err == nil {
				t.Fatalf("ValidateManifest() succeeded, want code %s", tc.wantCode)
			}
			if got := CodeOf(err); got != tc.wantCode {
				t.Fatalf("ValidateManifest() code = %q (%v), want %q", got, err, tc.wantCode)
			}
			if validated != nil {
				t.Fatalf("ValidateManifest() returned a manifest alongside error %v", err)
			}
		})
	}
}

func TestValidateManifestDefaultsAndIndexes(t *testing.T) {
	manifest := baseManifest()
	firstView(manifest)["platforms"] = []any{}
	data, err := json.Marshal(manifest)
	if err != nil {
		t.Fatal(err)
	}
	validated, err := ValidateManifest(data)
	if err != nil {
		t.Fatalf("ValidateManifest() error = %v", err)
	}
	view, ok := validated.View("sample.tab")
	if !ok {
		t.Fatal("declared view is not indexed")
	}
	if len(view.Platforms) != 1 || view.Platforms[0] != PlatformDesktop {
		t.Fatalf("platforms = %v, want [desktop]", view.Platforms)
	}
	command, ok := validated.Command("sample.ping")
	if !ok || command.Scope != CommandScopeSession {
		t.Fatalf("command index = %+v, %v", command, ok)
	}
	if !validated.DeclaresCapability(CapSessionObserve) {
		t.Fatal("session.observe should be declared")
	}
	if validated.DeclaresCapability(CapScreenView) {
		t.Fatal("screen.view must not be declared")
	}
	if _, ok := validated.Event("sample.events"); !ok {
		t.Fatal("event subscription is not indexed")
	}
}

func TestValidateManifestRejectsTrailingData(t *testing.T) {
	data, err := json.Marshal(baseManifest())
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ValidateManifest(append(data, []byte(" {}")...)); !IsCode(err, CodeInvalidManifest) {
		t.Fatalf("ValidateManifest() error = %v, want %s", err, CodeInvalidManifest)
	}
	if _, err := ValidateManifest([]byte(`{"manifest_version":1}`)); !IsCode(err, CodeInvalidID) {
		t.Fatalf("ValidateManifest() error = %v, want %s", err, CodeInvalidID)
	}
}

func TestLoadManifestReadsBoundFile(t *testing.T) {
	dir := testDir(t)
	path := filepath.Join(dir, "plugin.json")
	data, err := json.Marshal(baseManifest())
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadManifest(path); err != nil {
		t.Fatalf("LoadManifest() error = %v", err)
	}
	oversized := make([]byte, MaxManifestBytes+1)
	if err := os.WriteFile(path, oversized, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadManifest(path); !IsCode(err, CodeLimitExceeded) {
		t.Fatalf("LoadManifest(oversized) error = %v, want %s", err, CodeLimitExceeded)
	}
}
