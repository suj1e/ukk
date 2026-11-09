package app

import (
	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
)

// renderWorldNode lays out one corelayout node. renderBlock draws a block;
// focused is the view's focused block id, used for stacks.
func renderWorldNode(c *ui.Context, node corelayout.Node, focused string, renderBlock func(c *ui.Context, block corelayout.Node)) {
	switch node.Type {
	case corelayout.TypeBlock:
		renderBlock(c, node)
	case corelayout.TypeStack:
		for i := range node.Children {
			if node.Children[i].ID == focused {
				renderWorldNode(c, node.Children[i], focused, renderBlock)
				return
			}
		}
		if len(node.Children) > 0 {
			renderWorldNode(c, node.Children[0], focused, renderBlock)
		}
	case corelayout.TypeRow, corelayout.TypeColumn:
		horizontal := node.Type == corelayout.TypeRow
		if horizontal {
			ui.Row(c).Fill().Gap(worldGap(node.Gap)).Children(func() {
				for i := range node.Children {
					renderWorldChild(c, node.Children[i], focused, renderBlock, true)
				}
			})
			return
		}
		ui.Column(c).Fill().Gap(worldGap(node.Gap)).Children(func() {
			for i := range node.Children {
				renderWorldChild(c, node.Children[i], focused, renderBlock, false)
			}
		})
	default:
		// Unknown node kinds are not drawn; the contract validates known ones.
	}
}

// renderWorldChild gives a child its declared size or an equal share.
func renderWorldChild(c *ui.Context, node corelayout.Node, focused string, renderBlock func(*ui.Context, corelayout.Node), horizontal bool) {
	box := ui.Box(c)
	if node.Size != nil {
		switch {
		case node.Size.Px != nil:
			if horizontal {
				box.Width(*node.Size.Px)
			} else {
				box.Height(*node.Size.Px)
			}
		case node.Size.Weight != nil:
			box.Grow(*node.Size.Weight)
		}
	} else {
		box.Grow(1)
	}
	box.Children(func() {
		renderWorldNode(c, node, focused, renderBlock)
	})
}

func worldGap(gap *float32) float32 {
	if gap == nil {
		return 0
	}
	return *gap
}
