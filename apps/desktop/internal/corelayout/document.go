// Package corelayout models the machine-owned herdr-app document that
// herdr-core stores through doc.get and doc.set.
//
// The JSON field names and shapes mirror the upstream Rust types
// (herdr-app/src/mirror.rs and src/layout.rs). Decoding preserves unknown
// fields on nodes, floats, content and the style objects so a document written
// by a newer core can be read, validated and written back without losing data.
// The document, space and tab objects are closed by the world contract: unknown
// fields there are kept on decode but rejected by Validate.
package corelayout

import (
	"encoding/json"
	"fmt"
)

// Document is the root of the herdr-app world document.
type Document struct {
	// Spaces is ordered; the order is the order the user arranged them in.
	Spaces []SharedSpace `json:"spaces"`
	// Extra holds fields this version does not know. Validate rejects them:
	// the document object is closed.
	Extra map[string]json.RawMessage `json:"-"`
}

// SharedSpace is one machine-owned space.
type SharedSpace struct {
	ID     string      `json:"id"`
	Name   string      `json:"name"`
	Folder string      `json:"folder"`
	Tabs   []SharedTab `json:"tabs"`
	// Extra holds fields this version does not know. Validate rejects them:
	// a space is closed.
	Extra map[string]json.RawMessage `json:"-"`
}

// SharedTab is one tab inside a space and its pane tree.
type SharedTab struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Root Node   `json:"root"`
	// Extra holds fields this version does not know. Validate rejects them:
	// a tab is closed.
	Extra map[string]json.RawMessage `json:"-"`
}

// MarshalJSON encodes a document with "spaces" always present as an array.
func (d Document) MarshalJSON() ([]byte, error) {
	spaces := d.Spaces
	if spaces == nil {
		spaces = []SharedSpace{}
	}
	return object{"spaces": spaces}.marshal(d.Extra)
}

// UnmarshalJSON decodes a document and keeps unknown top-level fields.
func (d *Document) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode document: %w", err)
	}
	var out Document
	if err := takeValue(raw, "spaces", &out.Spaces); err != nil {
		return fmt.Errorf("corelayout: decode document: %w", err)
	}
	out.Extra = extras(raw)
	*d = out
	return nil
}

// MarshalJSON encodes a space with its required fields always present.
func (s SharedSpace) MarshalJSON() ([]byte, error) {
	tabs := s.Tabs
	if tabs == nil {
		tabs = []SharedTab{}
	}
	return object{
		"id":     s.ID,
		"name":   s.Name,
		"folder": s.Folder,
		"tabs":   tabs,
	}.marshal(s.Extra)
}

// UnmarshalJSON decodes a space and keeps unknown fields.
func (s *SharedSpace) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode space: %w", err)
	}
	var out SharedSpace
	for _, field := range []struct {
		key string
		dst any
	}{
		{"id", &out.ID},
		{"name", &out.Name},
		{"folder", &out.Folder},
		{"tabs", &out.Tabs},
	} {
		if err := takeValue(raw, field.key, field.dst); err != nil {
			return fmt.Errorf("corelayout: decode space: %w", err)
		}
	}
	out.Extra = extras(raw)
	*s = out
	return nil
}

// MarshalJSON encodes a tab with "root" always present.
func (t SharedTab) MarshalJSON() ([]byte, error) {
	return object{
		"id":   t.ID,
		"name": t.Name,
		"root": t.Root,
	}.marshal(t.Extra)
}

// UnmarshalJSON decodes a tab and keeps unknown fields. A missing root falls
// back to the same empty row the upstream decoder creates.
func (t *SharedTab) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode tab: %w", err)
	}
	var out SharedTab
	if err := takeValue(raw, "id", &out.ID); err != nil {
		return fmt.Errorf("corelayout: decode tab: %w", err)
	}
	if err := takeValue(raw, "name", &out.Name); err != nil {
		return fmt.Errorf("corelayout: decode tab: %w", err)
	}
	if body, ok := raw["root"]; ok {
		delete(raw, "root")
		if err := json.Unmarshal(body, &out.Root); err != nil {
			return fmt.Errorf("corelayout: decode tab root: %w", err)
		}
	} else {
		out.Root = emptyRoot()
	}
	out.Extra = extras(raw)
	*t = out
	return nil
}

func emptyRoot() Node {
	return Node{Type: TypeRow, Children: []Node{}}
}
