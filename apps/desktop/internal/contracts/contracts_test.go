// Package contracts validates the normative wire contracts under contracts/
// against their schemas and the Go types that implement them.
package contracts

import (
	"bytes"
	"encoding/json"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/suj1e/ukk/apps/desktop/internal/agents"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
	"github.com/suj1e/ukk/apps/desktop/internal/hc"
)

const baseURL = "https://contracts.ukk.dev/"

type manifest struct {
	Version  int `json:"version"`
	Fixtures []struct {
		File   string `json:"file"`
		Schema string `json:"schema"`
		Go     string `json:"go"`
	} `json:"fixtures"`
}

func contractsRoot(t *testing.T) string {
	t.Helper()
	root := filepath.Join("..", "..", "..", "..", "contracts")
	if _, err := os.Stat(filepath.Join(root, "manifest.json")); err != nil {
		t.Fatalf("contracts root: %v", err)
	}
	return root
}

func schemaURL(t *testing.T, root, path string) string {
	t.Helper()
	rel, err := filepath.Rel(root, path)
	if err != nil {
		t.Fatalf("relative schema path: %v", err)
	}
	return baseURL + filepath.ToSlash(rel)
}

func readJSON(t *testing.T, path string) any {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("decode %s: %v", path, err)
	}
	return doc
}

func TestSchemasCompileAndFixturesMatch(t *testing.T) {
	root := contractsRoot(t)

	var schemas []string
	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !entry.IsDir() && strings.HasSuffix(path, ".schema.json") {
			schemas = append(schemas, path)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("walk contracts: %v", err)
	}
	if len(schemas) < 4 {
		t.Fatalf("expected schemas for agent, bridge, core and remote; found %d", len(schemas))
	}

	compiler := jsonschema.NewCompiler()
	for _, path := range schemas {
		if err := compiler.AddResource(schemaURL(t, root, path), readJSON(t, path)); err != nil {
			t.Fatalf("add %s: %v", filepath.Base(path), err)
		}
	}
	compiled := make(map[string]*jsonschema.Schema, len(schemas))
	for _, path := range schemas {
		url := schemaURL(t, root, path)
		schema, err := compiler.Compile(url)
		if err != nil {
			t.Fatalf("compile %s: %v", filepath.Base(path), err)
		}
		rel, _ := filepath.Rel(root, path)
		compiled[filepath.ToSlash(rel)] = schema
	}

	var doc manifest
	raw, err := os.ReadFile(filepath.Join(root, "manifest.json"))
	if err != nil {
		t.Fatalf("manifest: %v", err)
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatalf("manifest decode: %v", err)
	}
	if doc.Version != 1 {
		t.Fatalf("manifest version = %d", doc.Version)
	}

	for _, fixture := range doc.Fixtures {
		fixture := fixture
		t.Run(fixture.File, func(t *testing.T) {
			schema, ok := compiled[fixture.Schema]
			if !ok {
				t.Fatalf("fixture names unknown schema %q", fixture.Schema)
			}
			path := filepath.Join(root, filepath.FromSlash(fixture.File))
			value := readJSON(t, path)
			if err := schema.Validate(value); err != nil {
				t.Fatalf("fixture does not match %s: %v", fixture.Schema, err)
			}
			data, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			roundTrip(t, schema, fixture.Go, data)
		})
	}
}

// roundTrip decodes a fixture into the Go type the manifest names and proves
// the re-encoded form still satisfies the schema, so struct tags cannot drift
// from the contract.
func roundTrip(t *testing.T, schema *jsonschema.Schema, name string, data []byte) {
	t.Helper()
	var value any
	switch name {
	case "":
		return
	case "agents.Snapshot":
		var typed agents.Snapshot
		if err := json.Unmarshal(data, &typed); err != nil {
			t.Fatalf("decode agents.Snapshot: %v", err)
		}
		if typed.ID == "" || typed.Provider == "" || !typed.State.Valid() {
			t.Fatalf("snapshot lacks id/provider/state: %+v", typed)
		}
		value = typed
	case "agents.Event":
		var typed agents.Event
		if err := json.Unmarshal(data, &typed); err != nil {
			t.Fatalf("decode agents.Event: %v", err)
		}
		if typed.SessionID == "" || typed.Sequence == 0 || typed.Kind == "" {
			t.Fatalf("event lacks session/sequence/kind: %+v", typed)
		}
		value = typed
	case "bridge.Request":
		var typed bridge.Request
		if err := json.Unmarshal(data, &typed); err != nil {
			t.Fatalf("decode bridge.Request: %v", err)
		}
		if typed.ID == "" || typed.Method == "" {
			t.Fatalf("request lacks id/method: %+v", typed)
		}
		value = typed
	case "bridge.Response":
		var typed bridge.Response
		if err := json.Unmarshal(data, &typed); err != nil {
			t.Fatalf("decode bridge.Response: %v", err)
		}
		if typed.ID == "" || (len(typed.Result) == 0 && typed.Error == nil) {
			t.Fatalf("response lacks id or payload: %+v", typed)
		}
		value = typed
	case "hc.HelloResult":
		var typed hc.HelloResult
		if err := json.Unmarshal(data, &typed); err != nil {
			t.Fatalf("decode hc.HelloResult: %v", err)
		}
		if typed.Generation != 1 || !typed.Framed || typed.Version == "" {
			t.Fatalf("hello lacks generation/framed/version: %+v", typed)
		}
		value = typed
	case "hc.ListResult":
		var typed hc.ListResult
		if err := json.Unmarshal(data, &typed); err != nil {
			t.Fatalf("decode hc.ListResult: %v", err)
		}
		if len(typed.Terminals) == 0 || typed.Terminals[0].Address == "" {
			t.Fatalf("list lacks terminals: %+v", typed)
		}
		value = typed
	default:
		t.Fatalf("manifest names unknown Go type %q", name)
	}

	encoded, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("encode %s: %v", name, err)
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(encoded))
	if err != nil {
		t.Fatalf("decode re-encoded %s: %v", name, err)
	}
	if err := schema.Validate(doc); err != nil {
		t.Fatalf("re-encoded %s does not match its schema: %v", name, err)
	}
}

func TestAgentSnapshotRejectsUnknownField(t *testing.T) {
	root := contractsRoot(t)
	value := readJSON(t, filepath.Join(root, "agent", "v1", "fixtures", "snapshot-done.json"))
	object, ok := value.(map[string]any)
	if !ok {
		t.Fatalf("fixture is not an object: %T", value)
	}
	object["unexpected"] = true
	compiler := jsonschema.NewCompiler()
	path := filepath.Join(root, "agent", "v1", "snapshot.schema.json")
	if err := compiler.AddResource(schemaURL(t, root, path), readJSON(t, path)); err != nil {
		t.Fatal(err)
	}
	schema, err := compiler.Compile(schemaURL(t, root, path))
	if err != nil {
		t.Fatal(err)
	}
	if err := schema.Validate(object); err == nil {
		t.Fatal("schema accepted an unknown field")
	}
}
