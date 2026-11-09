package corelayout

import (
	"errors"
	"fmt"
	"strconv"
	"unicode/utf8"
)

// ErrInvalid wraps every validation failure.
var ErrInvalid = errors.New("corelayout: invalid")

// Limits keep a document from a newer or malicious writer bounded. Depth and
// child counts are deliberately far above what a person can arrange, while
// still bounding recursive decoding and rendering.
const (
	MaxSpaces       = 1024
	MaxTabsPerSpace = 1024
	MaxTabs         = 16384
	MaxNodes        = 100000
	MaxNodeDepth    = 64
	MaxNodeChildren = 256
	MaxPropsBytes   = 1 << 20

	// Field limits mirror the world contract's JSON schema.
	MaxIDLength     = 256
	MaxNameLength   = 256
	MaxFolderLength = 4096
	MaxTitleLength  = 256
)

// Validate reports whether the document can be rendered by this version.
//
// Space IDs and tab IDs must be non-empty; tab IDs must be unique across the
// document because the model addresses a tab by ID alone. Names and folders may
// be empty: the upstream model falls back to a derived name and a space folder
// is documented as possibly empty. Unknown fields are allowed on nodes, floats,
// content and style objects but not on the document, a space or a tab, which
// the world contract closes.
func (d *Document) Validate() error {
	if d == nil {
		return invalidf("nil document")
	}
	if len(d.Extra) > 0 {
		return invalidf("document has unknown fields")
	}
	if len(d.Spaces) > MaxSpaces {
		return invalidf("document has more than %d spaces", MaxSpaces)
	}

	spaceIDs := make(map[string]struct{}, len(d.Spaces))
	tabIDs := make(map[string]struct{})
	tabs := 0
	nodes := 0
	for i := range d.Spaces {
		space := &d.Spaces[i]
		if len(space.Extra) > 0 {
			return invalidf("space %d has unknown fields", i)
		}
		if space.ID == "" {
			return invalidf("space %d: id required", i)
		}
		if err := checkLen("space id", space.ID, MaxIDLength); err != nil {
			return err
		}
		if _, seen := spaceIDs[space.ID]; seen {
			return invalidf("duplicate space id %s", quoteID(space.ID))
		}
		spaceIDs[space.ID] = struct{}{}
		if err := checkLen("space name", space.Name, MaxNameLength); err != nil {
			return err
		}
		if err := checkLen("space folder", space.Folder, MaxFolderLength); err != nil {
			return err
		}
		if len(space.Tabs) > MaxTabsPerSpace {
			return invalidf("space %s has more than %d tabs", quoteID(space.ID), MaxTabsPerSpace)
		}
		for j := range space.Tabs {
			tab := &space.Tabs[j]
			if len(tab.Extra) > 0 {
				return invalidf("space %s tab %d has unknown fields", quoteID(space.ID), j)
			}
			if tab.ID == "" {
				return invalidf("space %s tab %d: id required", quoteID(space.ID), j)
			}
			if err := checkLen("tab id", tab.ID, MaxIDLength); err != nil {
				return err
			}
			if _, seen := tabIDs[tab.ID]; seen {
				return invalidf("duplicate tab id %s", quoteID(tab.ID))
			}
			tabIDs[tab.ID] = struct{}{}
			if err := checkLen("tab name", tab.Name, MaxNameLength); err != nil {
				return err
			}
			tabs++
			if tabs > MaxTabs {
				return invalidf("document has more than %d tabs", MaxTabs)
			}
			if err := validateNode(&tab.Root, 1, &nodes); err != nil {
				return fmt.Errorf("space %s tab %s: %w", quoteID(space.ID), quoteID(tab.ID), err)
			}
		}
	}
	return nil
}

// Validate reports whether a node tree is within the layout limits.
func (n *Node) Validate() error {
	if n == nil {
		return invalidf("nil node")
	}
	nodes := 0
	return validateNode(n, 1, &nodes)
}

func validateNode(n *Node, depth int, nodes *int) error {
	if n == nil {
		return invalidf("nil node")
	}
	if depth > MaxNodeDepth {
		return invalidf("node nesting exceeds %d", MaxNodeDepth)
	}
	*nodes++
	if *nodes > MaxNodes {
		return invalidf("document has more than %d nodes", MaxNodes)
	}
	if err := checkLen("node id", n.ID, MaxIDLength); err != nil {
		return err
	}
	if n.Type == "" {
		return invalidf("node type required")
	}
	switch n.Type {
	case TypeBlock:
		if n.Content == nil {
			return invalidf("block content required")
		}
		if err := n.Content.Validate(); err != nil {
			return err
		}
	case TypeRow, TypeColumn, TypeStack:
	default:
		return invalidf("unknown node type %q", quoteID(n.Type))
	}
	if n.Active != nil {
		if err := checkLen("stack active id", *n.Active, MaxIDLength); err != nil {
			return err
		}
	}
	if n.Header != nil {
		if err := n.Header.Validate(); err != nil {
			return err
		}
	}
	if n.Size != nil {
		if err := n.Size.Validate(); err != nil {
			return err
		}
	}
	if n.Min != nil && *n.Min < 0 {
		return invalidf("node min must not be negative")
	}
	if n.Max != nil && *n.Max < 0 {
		return invalidf("node max must not be negative")
	}
	if n.Min != nil && n.Max != nil && *n.Min > *n.Max {
		return invalidf("node min exceeds max")
	}
	if n.Title != nil {
		if err := checkLen("node title", *n.Title, MaxTitleLength); err != nil {
			return err
		}
	}
	if n.Radius != nil && *n.Radius < 0 {
		return invalidf("node radius must not be negative")
	}
	if n.Gap != nil && *n.Gap < 0 {
		return invalidf("node gap must not be negative")
	}
	if n.Shadow != nil && !validShadow(*n.Shadow) {
		return invalidf("unknown shadow %q", quoteID(*n.Shadow))
	}
	if n.Background != nil {
		if err := n.Background.Validate(); err != nil {
			return err
		}
	}
	if n.Border != nil {
		if err := n.Border.Validate(); err != nil {
			return err
		}
	}
	if n.Divider != nil {
		if err := n.Divider.Validate(); err != nil {
			return err
		}
	}
	if n.Focus != nil {
		if err := n.Focus.Validate(); err != nil {
			return err
		}
	}
	if n.Padding != nil {
		if err := n.Padding.Validate(); err != nil {
			return err
		}
	}
	if len(n.Children) > MaxNodeChildren {
		return invalidf("node has more than %d children", MaxNodeChildren)
	}
	for i := range n.Children {
		if err := validateNode(&n.Children[i], depth+1, nodes); err != nil {
			return err
		}
	}
	if len(n.Floating) > MaxNodeChildren {
		return invalidf("node has more than %d floating blocks", MaxNodeChildren)
	}
	for i := range n.Floating {
		if err := n.Floating[i].Validate(); err != nil {
			return err
		}
	}
	return nil
}

// Validate reports whether content names exactly one pane source.
func (c *Content) Validate() error {
	if c == nil {
		return invalidf("nil content")
	}
	variants := 0
	if c.Terminal != nil {
		variants++
		if *c.Terminal == "" {
			return invalidf("terminal address required")
		}
		if err := checkLen("terminal address", *c.Terminal, MaxIDLength); err != nil {
			return err
		}
	}
	if c.Mod != nil {
		variants++
		if *c.Mod == "" {
			return invalidf("mod name required")
		}
		if err := checkLen("mod name", *c.Mod, MaxIDLength); err != nil {
			return err
		}
	}
	if c.Tab != nil {
		variants++
		if *c.Tab == "" {
			return invalidf("context tab id required")
		}
		if err := checkLen("context tab id", *c.Tab, MaxIDLength); err != nil {
			return err
		}
	}
	if c.Agent != nil {
		variants++
		if *c.Agent == "" {
			return invalidf("agent id required")
		}
		if err := checkLen("agent id", *c.Agent, MaxIDLength); err != nil {
			return err
		}
	}
	if variants != 1 {
		return invalidf("content must have exactly one of terminal, mod, tab or agent")
	}
	if c.Pane != nil {
		if c.Tab == nil {
			return invalidf("content pane requires a tab")
		}
		if err := c.Pane.Validate(); err != nil {
			return err
		}
	}
	if len(c.Props) > MaxPropsBytes {
		return invalidf("content props exceed %d bytes", MaxPropsBytes)
	}
	return nil
}

// Validate reports whether a floating block is bounded and well formed.
func (f *Float) Validate() error {
	if f == nil {
		return invalidf("nil floating block")
	}
	if f.ID == "" {
		return invalidf("floating block id required")
	}
	if err := checkLen("floating block id", f.ID, MaxIDLength); err != nil {
		return err
	}
	if f.Content == nil {
		return invalidf("floating block %s: content required", quoteID(f.ID))
	}
	if err := f.Content.Validate(); err != nil {
		return err
	}
	if f.Edge != nil && !validEdge(*f.Edge) {
		return invalidf("floating block %s: unknown edge %q", quoteID(f.ID), quoteID(*f.Edge))
	}
	if f.Align != nil && !validAlign(*f.Align) {
		return invalidf("floating block %s: unknown align %q", quoteID(f.ID), quoteID(*f.Align))
	}
	if f.Peek != nil && *f.Peek < 0 {
		return invalidf("floating block %s: peek must not be negative", quoteID(f.ID))
	}
	return nil
}

// Validate reports whether a pane frame is well formed.
func (p *PaneFrame) Validate() error {
	if p == nil {
		return invalidf("nil pane frame")
	}
	if p.Gap != nil && *p.Gap < 0 {
		return invalidf("pane frame gap must not be negative")
	}
	if p.Radius != nil && *p.Radius < 0 {
		return invalidf("pane frame radius must not be negative")
	}
	if p.Shadow != nil && !validShadow(*p.Shadow) {
		return invalidf("pane frame: unknown shadow %q", quoteID(*p.Shadow))
	}
	if p.Header != nil {
		if err := p.Header.Validate(); err != nil {
			return err
		}
	}
	if p.Background != nil {
		if err := p.Background.Validate(); err != nil {
			return err
		}
	}
	if p.Border != nil {
		if err := p.Border.Validate(); err != nil {
			return err
		}
	}
	if p.Focus != nil {
		if err := p.Focus.Validate(); err != nil {
			return err
		}
	}
	return nil
}

// Validate reports whether a header names exactly one content source.
func (h *Header) Validate() error {
	if h == nil {
		return invalidf("nil header")
	}
	if h.Content == nil {
		return invalidf("header content required")
	}
	if err := h.Content.Validate(); err != nil {
		return err
	}
	if h.Reveal != nil && *h.Reveal != RevealAlways && *h.Reveal != RevealHover {
		return invalidf("header: unknown reveal %q", quoteID(*h.Reveal))
	}
	if h.Height != nil && *h.Height < 0 {
		return invalidf("header height must not be negative")
	}
	if h.Position != nil && *h.Position != PositionTop && *h.Position != PositionBottom {
		return invalidf("header: unknown position %q", quoteID(*h.Position))
	}
	return nil
}

// Validate reports whether header content is the builtin strip or one mod.
func (h *HeaderContent) Validate() error {
	if h == nil {
		return invalidf("nil header content")
	}
	variants := 0
	if h.Builtin != nil {
		variants++
		if *h.Builtin == "" {
			return invalidf("header builtin name required")
		}
	}
	if h.Mod != nil {
		variants++
		if *h.Mod == "" {
			return invalidf("header mod name required")
		}
		if err := checkLen("header mod name", *h.Mod, MaxIDLength); err != nil {
			return err
		}
	}
	if variants != 1 {
		return invalidf("header content must have exactly one of builtin or mod")
	}
	if len(h.Props) > MaxPropsBytes {
		return invalidf("header props exceed %d bytes", MaxPropsBytes)
	}
	return nil
}

// Validate reports whether a background names exactly one paint source.
func (b *Background) Validate() error {
	if b == nil {
		return invalidf("nil background")
	}
	variants := 0
	if b.Color != nil {
		variants++
		if *b.Color == "" {
			return invalidf("background color required")
		}
	}
	if b.Gradient != nil {
		variants++
		if len(b.Gradient) != 2 {
			return invalidf("background gradient needs exactly two colors")
		}
		for _, color := range b.Gradient {
			if color == "" {
				return invalidf("background gradient color required")
			}
		}
	}
	if b.Image != nil {
		variants++
		if *b.Image == "" {
			return invalidf("background image path required")
		}
	}
	if variants != 1 {
		return invalidf("background must have exactly one of color, gradient or image")
	}
	if b.Fit != nil && *b.Fit != FitCover && *b.Fit != FitContain {
		return invalidf("background: unknown fit %q", quoteID(*b.Fit))
	}
	return nil
}

// Validate reports whether a line is a valid stroke.
func (l *Line) Validate() error {
	if l == nil {
		return invalidf("nil line")
	}
	if l.Width != nil && *l.Width < 0 {
		return invalidf("line width must not be negative")
	}
	if l.Color != nil && *l.Color == "" {
		return invalidf("line color required")
	}
	return nil
}

// Validate reports whether a focus look is valid.
func (f *FocusLook) Validate() error {
	if f == nil {
		return invalidf("nil focus look")
	}
	if f.Background != nil && *f.Background == "" {
		return invalidf("focus background required")
	}
	if f.Border != nil {
		if err := f.Border.Validate(); err != nil {
			return err
		}
	}
	return nil
}

// Validate reports whether a size names exactly one axis.
func (s *Size) Validate() error {
	if s == nil {
		return invalidf("nil size")
	}
	variants := 0
	if s.Px != nil {
		variants++
		if *s.Px < 0 {
			return invalidf("size px must not be negative")
		}
	}
	if s.Weight != nil {
		variants++
		if *s.Weight < 0 {
			return invalidf("size weight must not be negative")
		}
	}
	if variants != 1 {
		return invalidf("size must have exactly one of px or weight")
	}
	return nil
}

// Validate reports whether padding is one non-negative number or a
// non-negative [vertical, horizontal] pair.
func (p *Padding) Validate() error {
	if p == nil {
		return invalidf("nil padding")
	}
	variants := 0
	if p.All != nil {
		variants++
		if *p.All < 0 {
			return invalidf("padding must not be negative")
		}
	}
	if p.Axes != nil {
		variants++
		if p.Axes[0] < 0 || p.Axes[1] < 0 {
			return invalidf("padding must not be negative")
		}
	}
	if variants != 1 {
		return invalidf("padding must be one number or a two-element array")
	}
	return nil
}

func invalidf(format string, args ...any) error {
	return fmt.Errorf("%w: %s", ErrInvalid, fmt.Sprintf(format, args...))
}

func checkLen(what, value string, max int) error {
	if utf8.RuneCountInString(value) > max {
		return invalidf("%s exceeds %d characters", what, max)
	}
	return nil
}

// quoteID bounds a caller-controlled string before it enters an error message.
func quoteID(value string) string {
	const limit = 64
	if utf8.RuneCountInString(value) > limit {
		runes := []rune(value)
		value = string(runes[:limit]) + "..."
	}
	return strconv.Quote(value)
}

func validEdge(value string) bool {
	switch value {
	case EdgeTop, EdgeRight, EdgeBottom, EdgeLeft:
		return true
	default:
		return false
	}
}

func validAlign(value string) bool {
	switch value {
	case AlignStart, AlignCenter, AlignEnd:
		return true
	default:
		return false
	}
}

func validShadow(value string) bool {
	switch value {
	case ShadowSmall, ShadowMedium, ShadowLarge:
		return true
	default:
		return false
	}
}
