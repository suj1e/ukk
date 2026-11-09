// Package workspace composes navigation and pane geometry. It renders the
// product's layout but owns no durable state and performs no runtime action.
package workspace

import (
	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	domain "github.com/suj1e/ukk/apps/desktop/internal/workspace"
)

// PaneSizes maps a split's pane ID to its current first-pane percentage. The
// caller keeps the map across frames; Layout updates it as the user drags.
type PaneSizes map[string]*float32

// Size returns the stored percentage for a split, creating it from the saved
// ratio when absent.
func (s PaneSizes) Size(node *domain.PaneNode) *float32 {
	if p, ok := s[node.ID]; ok {
		return p
	}
	value := node.Ratio * 100
	if value < 15 {
		value = 50
	}
	p := new(float32)
	*p = value
	s[node.ID] = p
	return p
}

// Layout renders a validated pane tree. render builds one leaf; grow is the
// element's growth factor so nested splits fill their parent.
func Layout(c *ui.Context, node *domain.PaneNode, sizes PaneSizes,
	onRatio func(nodeID string, percent float32),
	render func(c *ui.Context, leaf *domain.PaneNode, grow float32)) ui.Element {
	if node == nil {
		return ui.Box(c)
	}
	if node.Kind == domain.KindLeaf {
		box := ui.Box(c).Grow(1).Fill()
		box.Children(func() { render(c, node, 1) })
		return box
	}
	pct := sizes.Size(node)
	var el ui.Element
	switch node.Split {
	case domain.SplitColumn:
		el = ui.SplitVerticalPercent(c, pct, func() {
			Layout(c, node.First, sizes, onRatio, render)
		}, func() {
			Layout(c, node.Second, sizes, onRatio, render)
		})
	default:
		el = ui.SplitPercent(c, pct, func() {
			Layout(c, node.First, sizes, onRatio, render)
		}, func() {
			Layout(c, node.Second, sizes, onRatio, render)
		})
	}
	if el.Changed() && onRatio != nil {
		onRatio(node.ID, *pct)
	}
	return el.Grow(1).Fill()
}

// PaneHeader names a pane and holds its actions. focus, when given, makes
// the title area take the pane's focus.
func PaneHeader(c *ui.Context, title, subtitle string, actions func(), focus ...func()) {
	theme := c.Theme()
	ui.Row(c).FillWidth().Height(30).PaddingX(10).Gap(8).
		AlignItems(ui.Center).Background(kit.Card(theme)).
		BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).Children(func() {
		if len(focus) > 0 && focus[0] != nil {
			onFocus := focus[0]
			area := ui.ButtonBase(c).Label(title).FillHeight()
			area.Children(func() { paneTitle(c, title, subtitle) })
			if area.Clicked() {
				onFocus()
			}
		} else {
			paneTitle(c, title, subtitle)
		}
		ui.Spacer(c)
		if actions != nil {
			actions()
		}
	})
}

// paneTitle is the title and subtitle of a PaneHeader, alone or in its
// clickable area.
func paneTitle(c *ui.Context, title, subtitle string) {
	theme := c.Theme()
	ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Text(c, title).FontSize(12).FontWeight(600).NoWrap()
		if subtitle != "" {
			ui.Text(c, subtitle).FontSize(10).TextColor(theme.TextMuted).NoWrap().Ellipsis(subtitle)
		}
	})
}

// TabBar shows the Session's tabs with a close action per tab. newMenu
// fills the new-tab menu; without it the + button opens a terminal.
func TabBar(c *ui.Context, tabs []domain.Tab, activeID string,
	onSelect func(id string), onClose func(id string), onNew func(),
	newMenu func(*ui.Menu), actions func(c *ui.Context)) {
	theme := c.Theme()
	ui.Row(c).FillWidth().Height(42).PaddingX(10).Gap(3).
		AlignItems(ui.Center).Background(theme.Surface).
		BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).
		Role(ui.RoleTabList).Children(func() {
		for _, tab := range tabs {
			selected := tab.ID == activeID
			row := ui.Row(c).Gap(6).AlignItems(ui.Center).
				Height(29).PaddingX(10).Radius(7).
				Role(ui.RoleTab).AccessSelected(selected)
			if selected {
				row.Background(theme.SurfaceHover).Border(1, theme.Border)
			}
			row.Children(func() {
				label := ui.ButtonBase(c)
				label.Role(ui.RoleTab).AccessSelected(selected)
				label.Children(func() {
					ui.Text(c, tab.Title).FontSize(12).
						TextColor(boolColor(selected, theme.Text, theme.TextMuted))
				})
				if label.Clicked() && !selected {
					onSelect(tab.ID)
				}
				if len(tabs) > 1 {
					close := ui.ButtonBase(c).Label("Close "+tab.Title).
						Size(18, 18).Radius(4)
					close.Children(func() {
						ui.Text(c, "×").FontSize(13).TextColor(theme.TextMuted)
					})
					if close.Clicked() {
						onClose(tab.ID)
					}
				}
			})
		}
		ui.Spacer(c)
		if newMenu != nil {
			ui.PopoverMenuButton(c, "+", newMenu).Label("New tab").
				PaddingX(7).PaddingY(3).FontSize(13)
		} else {
			ui.Button(c, "+").Label("New terminal").PaddingX(7).PaddingY(3).FontSize(13).OnClick(onNew)
		}
		if actions != nil {
			actions(c)
		}
	})
}

func boolColor(on bool, yes, no ui.Color) ui.Color {
	if on {
		return yes
	}
	return no
}

// SessionRow is one selectable Session in the sidebar.
func SessionRow(c *ui.Context, title, subtitle string, status ui.Color, selected bool) bool {
	theme := c.Theme()
	row := ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).
		Padding(6, 9).Radius(6)
	if selected {
		row.Background(theme.SurfaceHover)
	}
	clicked := false
	row.Children(func() {
		button := ui.ButtonBase(c).FillWidth()
		button.Children(func() {
			ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
				ui.Box(c).Size(7, 7).Radius(999).Background(status)
				ui.Column(c).Grow(1).Gap(1).Children(func() {
					ui.Text(c, title).FontSize(12).FontWeight(500).NoWrap().
						Ellipsis(title).TextColor(boolColor(selected, theme.Text, theme.Text))
					if subtitle != "" {
						ui.Text(c, subtitle).FontSize(10).NoWrap().
							Ellipsis(subtitle).TextColor(theme.TextMuted)
					}
				})
			})
		})
		clicked = button.Clicked()
	})
	return clicked
}

// SidebarSection is a labeled group in the sidebar.
func SidebarSection(c *ui.Context, label string, action func()) {
	ui.Row(c).FillWidth().PaddingX(9).PaddingY(6).AlignItems(ui.Center).Children(func() {
		kit.SectionLabel(c, label)
		ui.Spacer(c)
		if action != nil {
			action()
		}
	})
}
