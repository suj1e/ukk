package corelayout

import (
	"encoding/json"
	"fmt"
)

// Node type names, matching the upstream `type` discriminator.
const (
	TypeBlock  = "block"
	TypeRow    = "row"
	TypeColumn = "column"
	TypeStack  = "stack"
)

// Float edges and alignments.
const (
	EdgeTop    = "top"
	EdgeRight  = "right"
	EdgeBottom = "bottom"
	EdgeLeft   = "left"

	AlignStart  = "start"
	AlignCenter = "center"
	AlignEnd    = "end"
)

// Background image fitting.
const (
	FitCover   = "cover"
	FitContain = "contain"
)

// Shadow sizes accepted by the layout.
const (
	ShadowSmall  = "sm"
	ShadowMedium = "md"
	ShadowLarge  = "lg"
)

// Header reveal and position values.
const (
	RevealAlways   = "always"
	RevealHover    = "hover"
	PositionTop    = "top"
	PositionBottom = "bottom"
)

// ClonedFrom records the layout a node was cloned from.
type ClonedFrom struct {
	Name    string `json:"name"`
	Version string `json:"version"`
}

// Node is one node of a tab's layout tree. It is a block (one pane), a row or
// column of children, or a stack of overlaid children.
type Node struct {
	ID         string      `json:"id"`
	ClonedFrom *ClonedFrom `json:"cloned_from,omitempty"`
	Type       string      `json:"type"`
	// Content is set on a block and identifies what the pane shows.
	Content *Content `json:"content,omitempty"`
	// Children is set on row, column and stack nodes.
	Children []Node `json:"children,omitempty"`
	// Active is the visible child of a stack.
	Active *string `json:"active,omitempty"`
	// Header styles a stack's tab header.
	Header *Header `json:"header,omitempty"`

	Size          *Size       `json:"size,omitempty"`
	Min           *float32    `json:"min,omitempty"`
	Max           *float32    `json:"max,omitempty"`
	Hidden        bool        `json:"hidden,omitempty"`
	Title         *string     `json:"title,omitempty"`
	Background    *Background `json:"background,omitempty"`
	Border        *Line       `json:"border,omitempty"`
	Radius        *float32    `json:"radius,omitempty"`
	Shadow        *string     `json:"shadow,omitempty"`
	Padding       *Padding    `json:"padding,omitempty"`
	Focus         *FocusLook  `json:"focus,omitempty"`
	Gap           *float32    `json:"gap,omitempty"`
	Divider       *Line       `json:"divider,omitempty"`
	Movable       *bool       `json:"movable,omitempty"`
	Closable      *bool       `json:"closable,omitempty"`
	Resizable     *bool       `json:"resizable,omitempty"`
	Floating      []Float     `json:"floating,omitempty"`
	Titlebar      *string     `json:"titlebar,omitempty"`
	TrafficLights *[2]float32 `json:"traffic_lights,omitempty"`
	Preview       *string     `json:"preview,omitempty"`
	TerminalMenu  *string     `json:"terminal_menu,omitempty"`
	Toast         *string     `json:"toast,omitempty"`
	WindowDrag    bool        `json:"window_drag,omitempty"`

	// Extra holds fields this version does not know. A newer core may add
	// look fields; they are written back unchanged.
	Extra map[string]json.RawMessage `json:"-"`
}

// Content identifies what a block shows. Exactly one variant is set.
type Content struct {
	// Terminal is a herdr-core terminal address.
	Terminal *string `json:"terminal,omitempty"`
	// Mod is a mod name; Props is mod-defined and opaque.
	Mod   *string         `json:"mod,omitempty"`
	Props json.RawMessage `json:"props,omitempty"`
	// Tab is another tab shown in place; Pane styles its panes.
	Tab  *string    `json:"tab,omitempty"`
	Pane *PaneFrame `json:"pane,omitempty"`
	// Agent is an agent's terminal shown at full size.
	Agent *string `json:"agent,omitempty"`

	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// Size is a fixed pixel size or a flex weight.
type Size struct {
	Px     *float32 `json:"px,omitempty"`
	Weight *float32 `json:"weight,omitempty"`
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// Padding is one number or [vertical, horizontal].
type Padding struct {
	All  *float32
	Axes *[2]float32
}

// Line is a border or divider stroke.
type Line struct {
	Width *float32 `json:"width,omitempty"`
	Color *string  `json:"color,omitempty"`
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// FocusLook is how a focused node (one containing the focused block) looks.
type FocusLook struct {
	Border     *Line   `json:"border,omitempty"`
	Background *string `json:"background,omitempty"`
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// Background is a theme color/hex string, a gradient, or an image.
type Background struct {
	Color    *string
	Gradient []string
	Angle    *float32
	Image    *string
	Fit      *string
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// HeaderContent is the builtin "tabs" strip or a mod.
type HeaderContent struct {
	Builtin *string
	Mod     *string
	Props   json.RawMessage
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// Header styles a tab's or a stack's header.
type Header struct {
	Content  *HeaderContent `json:"content,omitempty"`
	Reveal   *string        `json:"reveal,omitempty"`
	Height   *float32       `json:"height,omitempty"`
	Position *string        `json:"position,omitempty"`
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// PaneFrame styles the panes of a context block.
type PaneFrame struct {
	Header     *Header     `json:"header,omitempty"`
	Background *Background `json:"background,omitempty"`
	Shadow     *string     `json:"shadow,omitempty"`
	Border     *Line       `json:"border,omitempty"`
	Focus      *Line       `json:"focus,omitempty"`
	Radius     *float32    `json:"radius,omitempty"`
	Gap        *float32    `json:"gap,omitempty"`
	Lone       bool        `json:"lone,omitempty"`
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// Float is a block floating over a node, snapped to one of its edges.
type Float struct {
	ID      string   `json:"id"`
	Content *Content `json:"content,omitempty"`
	Edge    *string  `json:"edge,omitempty"`
	Align   *string  `json:"align,omitempty"`
	Peek    *float32 `json:"peek,omitempty"`
	Hidden  bool     `json:"hidden,omitempty"`
	// Extra holds fields this version does not know.
	Extra map[string]json.RawMessage `json:"-"`
}

// MarshalJSON encodes a node, always writing `id` and `type`.
func (n Node) MarshalJSON() ([]byte, error) {
	o := object{"id": n.ID, "type": n.Type}
	if n.ClonedFrom != nil {
		o["cloned_from"] = n.ClonedFrom
	}
	if n.Content != nil {
		o["content"] = n.Content
	}
	if n.Children != nil || n.Type == TypeRow || n.Type == TypeColumn || n.Type == TypeStack {
		children := n.Children
		if children == nil {
			children = []Node{}
		}
		o["children"] = children
	}
	if n.Active != nil {
		o["active"] = *n.Active
	}
	if n.Header != nil {
		o["header"] = n.Header
	}
	if n.Size != nil {
		o["size"] = n.Size
	}
	o.addFloat("min", n.Min)
	o.addFloat("max", n.Max)
	o.addBool("hidden", n.Hidden)
	o.addString("title", n.Title)
	if n.Background != nil {
		o["background"] = n.Background
	}
	if n.Border != nil {
		o["border"] = n.Border
	}
	o.addFloat("radius", n.Radius)
	o.addString("shadow", n.Shadow)
	if n.Padding != nil {
		o["padding"] = n.Padding
	}
	if n.Focus != nil {
		o["focus"] = n.Focus
	}
	o.addFloat("gap", n.Gap)
	if n.Divider != nil {
		o["divider"] = n.Divider
	}
	if n.Movable != nil {
		o["movable"] = *n.Movable
	}
	if n.Closable != nil {
		o["closable"] = *n.Closable
	}
	if n.Resizable != nil {
		o["resizable"] = *n.Resizable
	}
	if n.Floating != nil {
		o["floating"] = n.Floating
	}
	o.addString("titlebar", n.Titlebar)
	if n.TrafficLights != nil {
		o["traffic_lights"] = *n.TrafficLights
	}
	o.addString("preview", n.Preview)
	o.addString("terminal_menu", n.TerminalMenu)
	o.addString("toast", n.Toast)
	o.addBool("window_drag", n.WindowDrag)
	return o.marshal(n.Extra)
}

// UnmarshalJSON decodes a node and keeps unknown fields.
func (n *Node) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode node: %w", err)
	}
	var out Node
	for _, field := range []struct {
		key string
		dst any
	}{
		{"id", &out.ID},
		{"cloned_from", &out.ClonedFrom},
		{"type", &out.Type},
		{"content", &out.Content},
		{"children", &out.Children},
		{"active", &out.Active},
		{"header", &out.Header},
		{"size", &out.Size},
		{"min", &out.Min},
		{"max", &out.Max},
		{"hidden", &out.Hidden},
		{"title", &out.Title},
		{"background", &out.Background},
		{"border", &out.Border},
		{"radius", &out.Radius},
		{"shadow", &out.Shadow},
		{"padding", &out.Padding},
		{"focus", &out.Focus},
		{"gap", &out.Gap},
		{"divider", &out.Divider},
		{"movable", &out.Movable},
		{"closable", &out.Closable},
		{"resizable", &out.Resizable},
		{"floating", &out.Floating},
		{"titlebar", &out.Titlebar},
		{"traffic_lights", &out.TrafficLights},
		{"preview", &out.Preview},
		{"terminal_menu", &out.TerminalMenu},
		{"toast", &out.Toast},
		{"window_drag", &out.WindowDrag},
	} {
		if err := takeValue(raw, field.key, field.dst); err != nil {
			return fmt.Errorf("corelayout: decode node: %w", err)
		}
	}
	out.Extra = extras(raw)
	*n = out
	return nil
}

// MarshalJSON encodes content, always writing `tab` instead of the legacy
// `context` alias.
func (c Content) MarshalJSON() ([]byte, error) {
	o := object{}
	if c.Terminal != nil {
		o["terminal"] = *c.Terminal
	}
	if c.Mod != nil {
		o["mod"] = *c.Mod
	}
	o.addRaw("props", c.Props)
	if c.Tab != nil {
		o["tab"] = *c.Tab
	}
	if c.Pane != nil {
		o["pane"] = c.Pane
	}
	if c.Agent != nil {
		o["agent"] = *c.Agent
	}
	return o.marshal(c.Extra)
}

// UnmarshalJSON decodes content and accepts the legacy `context` alias for
// `tab`.
func (c *Content) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode content: %w", err)
	}
	var out Content
	for _, field := range []struct {
		key string
		dst any
	}{
		{"terminal", &out.Terminal},
		{"mod", &out.Mod},
		{"props", &out.Props},
		{"tab", &out.Tab},
		{"pane", &out.Pane},
		{"agent", &out.Agent},
	} {
		if err := takeValue(raw, field.key, field.dst); err != nil {
			return fmt.Errorf("corelayout: decode content: %w", err)
		}
	}
	if body, ok := raw["context"]; ok {
		delete(raw, "context")
		if out.Tab == nil {
			var tab string
			if err := json.Unmarshal(body, &tab); err != nil {
				return fmt.Errorf("corelayout: decode content context: %w", err)
			}
			out.Tab = &tab
		}
	}
	out.Extra = extras(raw)
	*c = out
	return nil
}

// MarshalJSON encodes a size.
func (s Size) MarshalJSON() ([]byte, error) {
	o := object{}
	o.addFloat("px", s.Px)
	o.addFloat("weight", s.Weight)
	return o.marshal(s.Extra)
}

// UnmarshalJSON decodes a size and keeps unknown fields.
func (s *Size) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode size: %w", err)
	}
	var out Size
	if err := takeValue(raw, "px", &out.Px); err != nil {
		return fmt.Errorf("corelayout: decode size: %w", err)
	}
	if err := takeValue(raw, "weight", &out.Weight); err != nil {
		return fmt.Errorf("corelayout: decode size: %w", err)
	}
	out.Extra = extras(raw)
	*s = out
	return nil
}

// MarshalJSON encodes padding as a number or a two-element array.
func (p Padding) MarshalJSON() ([]byte, error) {
	if p.All != nil {
		return json.Marshal(*p.All)
	}
	if p.Axes != nil {
		return json.Marshal(*p.Axes)
	}
	return []byte("null"), nil
}

// UnmarshalJSON decodes a number or [vertical, horizontal].
func (p *Padding) UnmarshalJSON(data []byte) error {
	var one float32
	if err := json.Unmarshal(data, &one); err == nil {
		*p = Padding{All: &one}
		return nil
	}
	var axes [2]float32
	if err := json.Unmarshal(data, &axes); err == nil {
		*p = Padding{Axes: &axes}
		return nil
	}
	return fmt.Errorf("corelayout: padding must be a number or [vertical, horizontal]")
}

// MarshalJSON encodes a line.
func (l Line) MarshalJSON() ([]byte, error) {
	o := object{}
	o.addFloat("width", l.Width)
	o.addString("color", l.Color)
	return o.marshal(l.Extra)
}

// UnmarshalJSON decodes a line and keeps unknown fields.
func (l *Line) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode line: %w", err)
	}
	var out Line
	if err := takeValue(raw, "width", &out.Width); err != nil {
		return fmt.Errorf("corelayout: decode line: %w", err)
	}
	if err := takeValue(raw, "color", &out.Color); err != nil {
		return fmt.Errorf("corelayout: decode line: %w", err)
	}
	out.Extra = extras(raw)
	*l = out
	return nil
}

// MarshalJSON encodes a focus look.
func (f FocusLook) MarshalJSON() ([]byte, error) {
	o := object{}
	if f.Border != nil {
		o["border"] = f.Border
	}
	o.addString("background", f.Background)
	return o.marshal(f.Extra)
}

// UnmarshalJSON decodes a focus look and keeps unknown fields.
func (f *FocusLook) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode focus look: %w", err)
	}
	var out FocusLook
	if err := takeValue(raw, "border", &out.Border); err != nil {
		return fmt.Errorf("corelayout: decode focus look: %w", err)
	}
	if err := takeValue(raw, "background", &out.Background); err != nil {
		return fmt.Errorf("corelayout: decode focus look: %w", err)
	}
	out.Extra = extras(raw)
	*f = out
	return nil
}

// MarshalJSON encodes a background.
func (b Background) MarshalJSON() ([]byte, error) {
	if b.Color != nil {
		return json.Marshal(*b.Color)
	}
	o := object{}
	if b.Gradient != nil {
		o["gradient"] = b.Gradient
	}
	o.addFloat("angle", b.Angle)
	o.addString("image", b.Image)
	o.addString("fit", b.Fit)
	return o.marshal(b.Extra)
}

// UnmarshalJSON decodes a color string, gradient or image and keeps unknown
// fields.
func (b *Background) UnmarshalJSON(data []byte) error {
	var color string
	if err := json.Unmarshal(data, &color); err == nil {
		*b = Background{Color: &color}
		return nil
	}
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode background: %w", err)
	}
	var out Background
	if err := takeValue(raw, "gradient", &out.Gradient); err != nil {
		return fmt.Errorf("corelayout: decode background: %w", err)
	}
	if err := takeValue(raw, "angle", &out.Angle); err != nil {
		return fmt.Errorf("corelayout: decode background: %w", err)
	}
	if err := takeValue(raw, "image", &out.Image); err != nil {
		return fmt.Errorf("corelayout: decode background: %w", err)
	}
	if err := takeValue(raw, "fit", &out.Fit); err != nil {
		return fmt.Errorf("corelayout: decode background: %w", err)
	}
	out.Extra = extras(raw)
	*b = out
	return nil
}

// MarshalJSON encodes header content as the builtin string or a mod object.
func (h HeaderContent) MarshalJSON() ([]byte, error) {
	if h.Builtin != nil {
		return json.Marshal(*h.Builtin)
	}
	o := object{}
	if h.Mod != nil {
		o["mod"] = *h.Mod
	}
	o.addRaw("props", h.Props)
	return o.marshal(h.Extra)
}

// UnmarshalJSON decodes header content and keeps unknown fields.
func (h *HeaderContent) UnmarshalJSON(data []byte) error {
	var builtin string
	if err := json.Unmarshal(data, &builtin); err == nil {
		*h = HeaderContent{Builtin: &builtin}
		return nil
	}
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode header content: %w", err)
	}
	var out HeaderContent
	if err := takeValue(raw, "mod", &out.Mod); err != nil {
		return fmt.Errorf("corelayout: decode header content: %w", err)
	}
	if err := takeValue(raw, "props", &out.Props); err != nil {
		return fmt.Errorf("corelayout: decode header content: %w", err)
	}
	out.Extra = extras(raw)
	*h = out
	return nil
}

// MarshalJSON encodes a header.
func (h Header) MarshalJSON() ([]byte, error) {
	o := object{}
	if h.Content != nil {
		o["content"] = h.Content
	}
	o.addString("reveal", h.Reveal)
	o.addFloat("height", h.Height)
	o.addString("position", h.Position)
	return o.marshal(h.Extra)
}

// UnmarshalJSON decodes a header and keeps unknown fields.
func (h *Header) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode header: %w", err)
	}
	var out Header
	if err := takeValue(raw, "content", &out.Content); err != nil {
		return fmt.Errorf("corelayout: decode header: %w", err)
	}
	if err := takeValue(raw, "reveal", &out.Reveal); err != nil {
		return fmt.Errorf("corelayout: decode header: %w", err)
	}
	if err := takeValue(raw, "height", &out.Height); err != nil {
		return fmt.Errorf("corelayout: decode header: %w", err)
	}
	if err := takeValue(raw, "position", &out.Position); err != nil {
		return fmt.Errorf("corelayout: decode header: %w", err)
	}
	out.Extra = extras(raw)
	*h = out
	return nil
}

// MarshalJSON encodes a pane frame.
func (p PaneFrame) MarshalJSON() ([]byte, error) {
	o := object{}
	if p.Header != nil {
		o["header"] = p.Header
	}
	if p.Background != nil {
		o["background"] = p.Background
	}
	o.addString("shadow", p.Shadow)
	if p.Border != nil {
		o["border"] = p.Border
	}
	if p.Focus != nil {
		o["focus"] = p.Focus
	}
	o.addFloat("radius", p.Radius)
	o.addFloat("gap", p.Gap)
	o.addBool("lone", p.Lone)
	return o.marshal(p.Extra)
}

// UnmarshalJSON decodes a pane frame and keeps unknown fields.
func (p *PaneFrame) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode pane frame: %w", err)
	}
	var out PaneFrame
	for _, field := range []struct {
		key string
		dst any
	}{
		{"header", &out.Header},
		{"background", &out.Background},
		{"shadow", &out.Shadow},
		{"border", &out.Border},
		{"focus", &out.Focus},
		{"radius", &out.Radius},
		{"gap", &out.Gap},
		{"lone", &out.Lone},
	} {
		if err := takeValue(raw, field.key, field.dst); err != nil {
			return fmt.Errorf("corelayout: decode pane frame: %w", err)
		}
	}
	out.Extra = extras(raw)
	*p = out
	return nil
}

// MarshalJSON encodes a floating block.
func (f Float) MarshalJSON() ([]byte, error) {
	o := object{"id": f.ID}
	if f.Content != nil {
		o["content"] = f.Content
	}
	o.addString("edge", f.Edge)
	o.addString("align", f.Align)
	o.addFloat("peek", f.Peek)
	o.addBool("hidden", f.Hidden)
	return o.marshal(f.Extra)
}

// UnmarshalJSON decodes a floating block and keeps unknown fields.
func (f *Float) UnmarshalJSON(data []byte) error {
	raw, err := decodeObject(data)
	if err != nil {
		return fmt.Errorf("corelayout: decode float: %w", err)
	}
	var out Float
	for _, field := range []struct {
		key string
		dst any
	}{
		{"id", &out.ID},
		{"content", &out.Content},
		{"edge", &out.Edge},
		{"align", &out.Align},
		{"peek", &out.Peek},
		{"hidden", &out.Hidden},
	} {
		if err := takeValue(raw, field.key, field.dst); err != nil {
			return fmt.Errorf("corelayout: decode float: %w", err)
		}
	}
	out.Extra = extras(raw)
	*f = out
	return nil
}
