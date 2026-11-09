package workspace

import "fmt"

// Layout limits keep a window renderable and a malicious or corrupt document
// bounded.
const (
	MaxLeaves    = 16
	MaxDepth     = 8
	MinRatio     = 0.15
	MaxRatio     = 0.85
	SplitRow     = "row"
	SplitColumn  = "column"
	KindLeaf     = "leaf"
	KindSplit    = "split"
	SurfaceChat  = "conversation"
	SurfaceTerm  = "terminal"
	SurfaceFiles = "files"
	SurfaceDiff  = "diff"
	SurfaceSpec  = "screen"
	SurfaceLog   = "activity"
)

// PaneNode is a validated binary split tree. A leaf shows one surface; a split
// holds two children and a ratio for the first.
type PaneNode struct {
	ID     string    `json:"id"`
	Kind   string    `json:"kind"`
	Split  string    `json:"split,omitempty"`
	Ratio  float32   `json:"ratio,omitempty"`
	First  *PaneNode `json:"first,omitempty"`
	Second *PaneNode `json:"second,omitempty"`

	Surface    string `json:"surface,omitempty"`
	AgentRunID string `json:"agent_run_id,omitempty"`
	// TerminalAddress shows a herdr-core terminal that no AgentRun owns, such
	// as a shell started from the Desktop. The core still owns the process.
	TerminalAddress string `json:"terminal_address,omitempty"`
}

// Tab groups panes under one title.
type Tab struct {
	ID    string    `json:"id"`
	Title string    `json:"title"`
	Root  *PaneNode `json:"root"`
}

// SessionLayout is a Session's saved presentation tree.
type SessionLayout struct {
	SessionID string `json:"session_id"`
	Revision  uint64 `json:"revision"`
	Tabs      []Tab  `json:"tabs"`
	ActiveTab string `json:"active_tab"`
	UpdatedMS int64  `json:"updated_ms"`
}

// NewLeaf returns a leaf pane for the given surface.
func NewLeaf(surface string) *PaneNode {
	return &PaneNode{ID: newID(), Kind: KindLeaf, Surface: surface}
}

// NewTerminalLeaf returns a terminal leaf that shows a core-owned terminal
// which no AgentRun owns.
func NewTerminalLeaf(address string) *PaneNode {
	return &PaneNode{ID: newID(), Kind: KindLeaf, Surface: SurfaceTerm, TerminalAddress: address}
}

// NewSplit returns a split holding first and second.
func NewSplit(direction string, ratio float32, first, second *PaneNode) *PaneNode {
	return &PaneNode{
		ID:     newID(),
		Kind:   KindSplit,
		Split:  direction,
		Ratio:  ratio,
		First:  first,
		Second: second,
	}
}

// Validate reports whether the tree is renderable and within limits.
func (n *PaneNode) Validate() error {
	if n == nil {
		return fmt.Errorf("%w: nil pane", ErrInvalid)
	}
	leaves := 0
	var walk func(*PaneNode, int) error
	walk = func(node *PaneNode, depth int) error {
		if node == nil {
			return fmt.Errorf("%w: nil pane child", ErrInvalid)
		}
		if depth > MaxDepth {
			return fmt.Errorf("%w: pane nesting exceeds %d", ErrInvalid, MaxDepth)
		}
		if node.ID == "" {
			return fmt.Errorf("%w: pane id required", ErrInvalid)
		}
		switch node.Kind {
		case KindLeaf:
			leaves++
			if leaves > MaxLeaves {
				return fmt.Errorf("%w: more than %d panes", ErrInvalid, MaxLeaves)
			}
			if node.First != nil || node.Second != nil {
				return fmt.Errorf("%w: leaf has children", ErrInvalid)
			}
			if node.Surface == "" {
				return fmt.Errorf("%w: leaf surface required", ErrInvalid)
			}
			switch node.Surface {
			case SurfaceChat, SurfaceTerm, SurfaceFiles, SurfaceDiff, SurfaceSpec, SurfaceLog:
			default:
				return fmt.Errorf("%w: unknown surface %q", ErrInvalid, node.Surface)
			}
		case KindSplit:
			if node.Split != SplitRow && node.Split != SplitColumn {
				return fmt.Errorf("%w: split must be row or column", ErrInvalid)
			}
			if node.Ratio < MinRatio || node.Ratio > MaxRatio {
				return fmt.Errorf("%w: ratio outside %.2f-%.2f", ErrInvalid, MinRatio, MaxRatio)
			}
			if err := walk(node.First, depth+1); err != nil {
				return err
			}
			return walk(node.Second, depth+1)
		default:
			return fmt.Errorf("%w: unknown pane kind %q", ErrInvalid, node.Kind)
		}
		return nil
	}
	return walk(n, 1)
}

// SplitLeaf replaces leafID with a split of that leaf and a new leaf.
func (n *PaneNode) SplitLeaf(leafID, direction string, ratio float32, surface string) (*PaneNode, error) {
	if err := n.Validate(); err != nil {
		return nil, err
	}
	if ratio < MinRatio || ratio > MaxRatio {
		return nil, fmt.Errorf("%w: ratio outside bounds", ErrInvalid)
	}
	var replace func(node *PaneNode) (*PaneNode, bool)
	replace = func(node *PaneNode) (*PaneNode, bool) {
		if node == nil {
			return nil, false
		}
		if node.Kind == KindLeaf {
			if node.ID != leafID {
				return node, false
			}
			first, second := node, NewLeaf(surface)
			return NewSplit(direction, ratio, first, second), true
		}
		if next, ok := replace(node.First); ok {
			node.First = next
			return node, true
		}
		if next, ok := replace(node.Second); ok {
			node.Second = next
			return node, true
		}
		return node, false
	}
	root, ok := replace(n)
	if !ok {
		return nil, ErrNotFound
	}
	if err := root.Validate(); err != nil {
		return nil, err
	}
	return root, nil
}

// CloseLeaf removes leafID and promotes its sibling.
func (n *PaneNode) CloseLeaf(leafID string) (*PaneNode, error) {
	if err := n.Validate(); err != nil {
		return nil, err
	}
	if n.Kind == KindLeaf {
		if n.ID == leafID {
			return nil, nil
		}
		return n, ErrNotFound
	}
	var walk func(node *PaneNode) (*PaneNode, bool, error)
	walk = func(node *PaneNode) (*PaneNode, bool, error) {
		if node.First != nil && node.First.Kind == KindLeaf && node.First.ID == leafID {
			return node.Second, true, nil
		}
		if node.Second != nil && node.Second.Kind == KindLeaf && node.Second.ID == leafID {
			return node.First, true, nil
		}
		if node.First != nil && node.First.Kind == KindSplit {
			next, ok, err := walk(node.First)
			if err != nil || ok {
				node.First = next
				return node, true, err
			}
		}
		if node.Second != nil && node.Second.Kind == KindSplit {
			next, ok, err := walk(node.Second)
			if err != nil || ok {
				node.Second = next
				return node, true, err
			}
		}
		return node, false, nil
	}
	root, ok, err := walk(n)
	if err != nil {
		return nil, err
	}
	if !ok {
		return nil, ErrNotFound
	}
	if root != nil {
		if err := root.Validate(); err != nil {
			return nil, err
		}
	}
	return root, nil
}

// Find returns the leaf with id.
func (n *PaneNode) Find(id string) *PaneNode {
	if n == nil {
		return nil
	}
	if n.ID == id && n.Kind == KindLeaf {
		return n
	}
	if leaf := n.First.Find(id); leaf != nil {
		return leaf
	}
	return n.Second.Find(id)
}

// Leaves lists the leaf panes in reading order.
func (n *PaneNode) Leaves() []*PaneNode {
	var out []*PaneNode
	var walk func(*PaneNode)
	walk = func(node *PaneNode) {
		if node == nil {
			return
		}
		if node.Kind == KindLeaf {
			out = append(out, node)
			return
		}
		walk(node.First)
		walk(node.Second)
	}
	walk(n)
	return out
}
