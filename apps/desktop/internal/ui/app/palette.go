package app

import (
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	domain "github.com/suj1e/ukk/apps/desktop/internal/workspace"
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
		{"New terminal", "shell in this Space", a.NewTerminal},
		{"New conversation tab", "chat with the selected Agent", func() { a.addTab(domain.SurfaceChat) }},
		{"New Session", "group Agent runs", func() {
			a.newSession.Open = true
			a.newSession.Err = ""
		}},
		{"New Space", "pick a project root", func() { a.newSpace.Open = true }},
		{"Split right", "conversation beside this pane", func() { a.splitPane(domain.SplitRow, domain.SurfaceChat) }},
		{"Split down", "conversation below this pane", func() { a.splitPane(domain.SplitColumn, domain.SurfaceChat) }},
		{"Close pane", "detach this view only", func() { a.closePane(a.activePane) }},
		{"Refresh herdr-core", "reconnect and list terminals", a.refreshTerminals},
		{"Settings", "devices, plugins, about", func() {
			a.settings.Open = true
			a.loadRemoteStatus()
		}},
	}
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
