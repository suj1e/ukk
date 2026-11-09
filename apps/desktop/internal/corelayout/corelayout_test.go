package corelayout

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

const fixture = `{
  "spaces": [
    {
      "id": "desk/web",
      "name": "Web",
      "folder": "/home/dev/projects/site",
      "tabs": [
        {
          "id": "desk/web/tab-dev",
          "name": "dev",
          "root": {
            "id": "desk/web/tab-dev/root",
            "type": "row",
            "gap": 4,
            "padding": [8, 12],
            "theme": "dark",
            "future_node": {"x": 1},
            "children": [
              {
                "id": "server",
                "type": "block",
                "title": "server",
                "size": {"weight": 2},
                "min": 120,
                "content": {"terminal": "desk/term-1", "future_content": true}
              },
              {
                "id": "side",
                "type": "column",
                "size": {"weight": 1},
                "children": [
                  {
                    "id": "shell",
                    "type": "block",
                    "content": {"context": "desk/web/tab-dev"}
                  },
                  {
                    "id": "files",
                    "type": "block",
                    "size": {"px": 260},
                    "border": {"width": 1, "color": "border"},
                    "focus": {"border": {"color": "accent"}, "background": "raised"},
                    "content": {
                      "mod": "files",
                      "props": {"root": "src", "future_prop": 3}
                    }
                  }
                ]
              }
            ],
            "floating": [
              {
                "id": "todo",
                "content": {"mod": "todo"},
                "edge": "left",
                "align": "start",
                "peek": 220,
                "hidden": true,
                "future_float": "kept"
              }
            ]
          }
        }
      ]
    },
    {
      "id": "desk/agents",
      "name": "Agents",
      "folder": "",
      "tabs": [
        {
          "id": "desk/agents/tab-omp",
          "name": "omp",
          "root": {
            "id": "omp-root",
            "type": "stack",
            "active": "omp-main",
            "header": {"content": "tabs", "height": 30, "position": "top"},
            "children": [
              {
                "id": "omp-main",
                "type": "block",
                "content": {"agent": "desk/omp-1"},
                "background": {"gradient": ["#111111", "#222222"], "angle": 90}
              },
              {
                "id": "omp-other",
                "type": "block",
                "hidden": true,
                "content": {
                  "tab": "desk/agents/tab-omp",
                  "pane": {
                    "gap": 8,
                    "radius": 10,
                    "lone": true,
                    "background": {"image": "waves.png", "fit": "contain"},
                    "border": {"width": 1, "color": "border"},
                    "focus": {"width": 1, "color": "accent"},
                    "header": {
                      "content": {"mod": "pane", "props": {"x": 1}},
                      "reveal": "hover",
                      "height": 30,
                      "extra": true
                    },
                    "future_pane": 7
                  }
                }
              }
            ]
          }
        }
      ]
    }
  ]
}`

func decodeFixture(t *testing.T, body string) Document {
	t.Helper()
	var doc Document
	if err := json.Unmarshal([]byte(body), &doc); err != nil {
		t.Fatalf("decode: %v", err)
	}
	return doc
}

func TestDocumentRoundTripPreservesUnknownFields(t *testing.T) {
	doc := decodeFixture(t, fixture)
	if err := doc.Validate(); err != nil {
		t.Fatalf("fixture must validate: %v", err)
	}

	first, err := json.Marshal(doc)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	var again Document
	if err := json.Unmarshal(first, &again); err != nil {
		t.Fatalf("re-decode: %v", err)
	}
	second, err := json.Marshal(again)
	if err != nil {
		t.Fatalf("re-encode: %v", err)
	}
	if string(first) != string(second) {
		t.Fatalf("round trip is not stable:\nfirst:  %s\nsecond: %s", first, second)
	}

	// Unknown node, content, float and pane-frame fields survive.
	root := again.Spaces[0].Tabs[0].Root
	if root.Extra["future_node"] == nil || root.Extra["theme"] == nil {
		t.Fatalf("unknown node fields lost: %v", root.Extra)
	}
	terminal := root.Children[0].Content
	if terminal.Extra["future_content"] == nil {
		t.Fatalf("unknown content field lost: %v", terminal.Extra)
	}
	if root.Floating[0].Extra["future_float"] == nil {
		t.Fatalf("unknown float field lost: %v", root.Floating[0].Extra)
	}
	pane := again.Spaces[1].Tabs[0].Root.Children[1].Content.Pane
	if pane.Extra["future_pane"] == nil {
		t.Fatalf("unknown pane frame field lost: %v", pane.Extra)
	}

	// The legacy `context` spelling decodes as tab and is never written back.
	contextBlock := root.Children[1].Children[0].Content
	if contextBlock.Tab == nil || *contextBlock.Tab != "desk/web/tab-dev" {
		t.Fatalf("context alias not decoded as tab: %#v", contextBlock)
	}
	if strings.Contains(string(first), `"context"`) {
		t.Fatalf("encoder wrote the legacy context field: %s", first)
	}
}

func TestDocumentFieldNamesAndDefaults(t *testing.T) {
	empty, err := json.Marshal(Document{})
	if err != nil {
		t.Fatalf("encode empty: %v", err)
	}
	if string(empty) != `{"spaces":[]}` {
		t.Fatalf("empty document = %s", empty)
	}

	doc := Document{Spaces: []SharedSpace{{
		ID: "s", Name: "n", Folder: "f",
		Tabs: []SharedTab{{ID: "t", Name: "n", Root: Node{
			ID: "root", Type: TypeBlock,
			Content: &Content{Terminal: new("term")},
		}}},
	}}}
	body, err := json.Marshal(doc)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	for _, field := range []string{`"spaces"`, `"id"`, `"name"`, `"folder"`, `"tabs"`, `"root"`} {
		if !strings.Contains(string(body), field) {
			t.Fatalf("encoded document lacks %s: %s", field, body)
		}
	}
	if strings.Contains(string(body), `"hidden"`) || strings.Contains(string(body), `"size"`) {
		t.Fatalf("encoder invented optional fields: %s", body)
	}
}

func TestDocumentValidationFailures(t *testing.T) {
	validTab := func() SharedTab {
		return SharedTab{ID: "tab", Name: "tab", Root: Node{
			ID: "pane", Type: TypeBlock,
			Content: &Content{Terminal: new("term")},
		}}
	}
	validDoc := func() Document {
		return Document{Spaces: []SharedSpace{{
			ID: "space", Name: "space", Folder: "/work", Tabs: []SharedTab{validTab()},
		}}}
	}

	deep := Node{ID: "leaf", Type: TypeBlock, Content: &Content{Terminal: new("term")}}
	for range MaxNodeDepth {
		deep = Node{ID: "row", Type: TypeRow, Children: []Node{deep}}
	}
	wide := Node{ID: "wide", Type: TypeRow}
	for range MaxNodeChildren + 1 {
		wide.Children = append(wide.Children, Node{
			ID: "leaf", Type: TypeBlock, Content: &Content{Terminal: new("term")},
		})
	}

	cases := []struct {
		name   string
		mutate func(*Document)
	}{
		{"empty space id", func(d *Document) { d.Spaces[0].ID = "" }},
		{"duplicate space id", func(d *Document) {
			d.Spaces = append(d.Spaces, d.Spaces[0])
		}},
		{"empty tab id", func(d *Document) { d.Spaces[0].Tabs[0].ID = "" }},
		{"duplicate tab id", func(d *Document) {
			second := SharedSpace{ID: "other", Name: "other", Folder: "/other", Tabs: []SharedTab{validTab()}}
			d.Spaces = append(d.Spaces, second)
		}},
		{"empty terminal address", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Content.Terminal = new("")
		}},
		{"block without content", func(d *Document) { d.Spaces[0].Tabs[0].Root.Content = nil }},
		{"unknown node type", func(d *Document) { d.Spaces[0].Tabs[0].Root.Type = "grid" }},
		{"deep tree", func(d *Document) { d.Spaces[0].Tabs[0].Root = deep }},
		{"wide tree", func(d *Document) { d.Spaces[0].Tabs[0].Root = wide }},
		{"unknown document field", func(d *Document) {
			d.Extra = map[string]json.RawMessage{"view": json.RawMessage(`{"active":"x"}`)}
		}},
		{"unknown space field", func(d *Document) {
			d.Spaces[0].Extra = map[string]json.RawMessage{"machine": json.RawMessage(`"x"`)}
		}},
		{"unknown tab field", func(d *Document) {
			d.Spaces[0].Tabs[0].Extra = map[string]json.RawMessage{"focused": json.RawMessage(`"p"`)}
		}},
		{"invalid size", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Size = &Size{Px: new(float32(1)), Weight: new(float32(1))}
		}},
		{"negative gap", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Gap = new(float32(-1))
		}},
		{"negative padding", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Padding = &Padding{Axes: &[2]float32{-1, 0}}
		}},
		{"min above max", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Min = new(float32(10))
			d.Spaces[0].Tabs[0].Root.Max = new(float32(9))
		}},
		{"bad float edge", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Floating = []Float{{
				ID: "float", Content: &Content{Mod: new("todo")}, Edge: new("middle"),
			}}
		}},
		{"empty float id", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Floating = []Float{{
				Content: &Content{Mod: new("todo")},
			}}
		}},
		{"bad gradient", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Background = &Background{Gradient: []string{"#111"}}
		}},
		{"two backgrounds", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Background = &Background{
				Color: new("canvas"), Image: new("waves.png"),
			}
		}},
		{"pane without tab", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Content.Pane = &PaneFrame{Gap: new(float32(2))}
		}},
		{"two content sources", func(d *Document) {
			d.Spaces[0].Tabs[0].Root.Content.Agent = new("agent")
		}},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			doc := validDoc()
			tc.mutate(&doc)
			if err := doc.Validate(); !errors.Is(err, ErrInvalid) {
				t.Fatalf("Validate() = %v, want ErrInvalid", err)
			}
		})
	}
}

func TestMinimalDocumentsAndEmptyNamesValidate(t *testing.T) {
	doc := Document{Spaces: []SharedSpace{{
		ID: "space",
		// Upstream falls back to a derived space name and documents an empty
		// space folder as valid.
		Name:   "",
		Folder: "",
		Tabs: []SharedTab{{ID: "tab", Name: "", Root: Node{
			// Node IDs may be empty for a node the app has not named.
			Type:     TypeRow,
			Children: []Node{},
		}}},
	}}}
	if err := doc.Validate(); err != nil {
		t.Fatalf("minimal document must validate: %v", err)
	}
}

func TestNodeValidationStandalone(t *testing.T) {
	node := Node{ID: "n", Type: TypeBlock, Content: &Content{Terminal: new("t")}}
	if err := node.Validate(); err != nil {
		t.Fatalf("valid node rejected: %v", err)
	}
	node.Content.Terminal = nil
	if err := node.Validate(); !errors.Is(err, ErrInvalid) {
		t.Fatalf("empty content accepted: %v", err)
	}
	var nilNode *Node
	if err := nilNode.Validate(); !errors.Is(err, ErrInvalid) {
		t.Fatalf("nil node accepted: %v", err)
	}
}
