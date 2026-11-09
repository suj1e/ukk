package app

import (
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
)

// paletteAction is one command in the palette. hint is the quiet right-hand
// text; run executes on the UI thread.
type paletteAction struct {
	title string
	hint  string
	run   func()
}

// OpenPalette opens the command palette over the current workspace.
func (a *App) OpenPalette() {
	a.palette.Open = true
	a.palette.Query = ""
}

func (a *App) paletteActions() []paletteAction {
	return []paletteAction{
		{"New Space", "pick a project folder", a.NewSpace},
		{"New tab", "shell in this Space", a.NewTerminal},
		{"New chat tab", "conversation pane", a.newConversationTab},
		{"New files tab", "repository files", func() { a.newModTab("files", "Files") }},
		{"New diff tab", "working-tree changes", func() { a.newModTab("changes", "Diff") }},
		{"Split right", "shell beside the focused pane", func() { a.splitActive(corelayout.TypeRow) }},
		{"Split down", "shell below the focused pane", func() { a.splitActive(corelayout.TypeColumn) }},
		{"Close pane", "detach this view only", a.CloseActivePane},
		{"Close tab", "detach this view only", func() { closeActiveTab(a) }},
		{"Rename current tab…", "edit the shared tab name", func() { renameActiveTab(a) }},
		{"Refresh herdr-core", "reconnect and reload the world", a.refreshTerminals},
		{"Settings", "devices, plugins, about", a.OpenSettings},
	}
}

func closeActiveTab(a *App) {
	tab, ok := a.activeTab()
	if !ok {
		return
	}
	a.closeTab(tab.ID)
}

func renameActiveTab(a *App) {
	space, ok := a.activeSpace()
	if !ok {
		return
	}
	tab, ok := a.activeTab()
	if !ok {
		return
	}
	a.openTabRename(space, tab)
}

func (a *App) paletteDialog(c *ui.Context) {
	if !a.palette.Open {
		return
	}
	form := &a.palette
	ui.Modal(c, &form.Open, func() {
		ui.Column(c).Width(560).Gap(10).Padding(14).Children(func() {
			ui.TextInput(c, &form.Query).Placeholder("Type a command…").FillWidth()
			query := strings.ToLower(strings.TrimSpace(form.Query))
			shown := 0
			ui.Scroll(c).MaxHeight(360).Children(func() {
				ui.Column(c).Gap(2).Children(func() {
					for _, action := range a.paletteActions() {
						action := action
						haystack := strings.ToLower(action.title + " " + action.hint)
						if query != "" && !strings.Contains(haystack, query) {
							continue
						}
						shown++
						button := ui.ButtonBase(c).FillWidth()
						button.Children(func() {
							row := ui.Row(c).FillWidth().Padding(7, 9).Gap(10).AlignItems(ui.Center)
							row.Children(func() {
								ui.Text(c, action.title).FontSize(13).FontWeight(500)
								ui.Spacer(c)
								if action.hint != "" {
									kit.Meta(c, action.hint)
								}
							})
						})
						if button.Clicked() {
							form.Open = false
							action.run()
						}
					}
				})
			})
			if shown == 0 {
				kit.Meta(c, "No matching command.")
			}
		})
	})
}
