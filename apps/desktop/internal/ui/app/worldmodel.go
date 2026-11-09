package app

import (
	"context"

	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
)

// coreWorld is the Desktop's view of the herdr-core document `herdr-app`.
// *world.World implements it; the UI mutates only through these methods and
// treats every returned tree as a copy. Closing or removing a pane, tab or
// space changes metadata only: it never stops a terminal.
type coreWorld interface {
	Spaces() []corelayout.SharedSpace
	ActiveSpace() (corelayout.SharedSpace, bool)
	ActiveTab() (corelayout.SharedTab, bool)
	Document() corelayout.Document

	SelectSpace(id string) bool
	SelectTab(spaceID, tabID string) bool
	FocusPane(tabID, paneID string) bool
	FocusedPane(tabID string) string

	AddSpace(space corelayout.SharedSpace) error
	AddTab(spaceID string, tab corelayout.SharedTab) error
	Split(spaceID, tabID, paneID, axis string, block corelayout.Node) error
	RemovePane(spaceID, tabID, paneID string) error
	RemoveTab(spaceID, tabID string) error
	RemoveSpace(spaceID string) error
	RenameSpace(spaceID, name string) error
	RenameTab(spaceID, tabID, name string) error

	Save(ctx context.Context) error
	SaveView() error
}

// focusedPaneID is the view's focused pane for a tab, or "".
func focusedPaneID(tab corelayout.SharedTab) string {
	for _, block := range tab.Root.Blocks() {
		if block.ID != "" {
			return block.ID
		}
	}
	return ""
}
