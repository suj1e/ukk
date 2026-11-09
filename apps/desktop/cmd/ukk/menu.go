package main

import (
	"github.com/egoist/mygo"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/app"
)

func buildMenu(application *app.App) *mygo.Menu {
	return mygo.NewMenu([]*mygo.MenuItem{
		{
			Role: mygo.RoleAppMenu,
			Submenu: []*mygo.MenuItem{
				{Role: mygo.RoleAbout},
				mygo.Separator(),
				{Label: "Settings…", Accelerator: "CmdOrCtrl+,", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.OpenSettings()
				}},
				mygo.Separator(),
				{Role: mygo.RoleHide},
				{Role: mygo.RoleHideOthers},
				{Role: mygo.RoleUnhide},
				mygo.Separator(),
				{Role: mygo.RoleQuit},
			},
		},
		{
			Label: "File",
			Submenu: []*mygo.MenuItem{
				{Label: "New Space", Accelerator: "CmdOrCtrl+Shift+N", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.NewSpace()
				}},
				mygo.Separator(),
				{Label: "New Terminal", Accelerator: "CmdOrCtrl+Shift+T", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.NewTerminal()
				}},
				{Label: "New Tab", Accelerator: "CmdOrCtrl+T", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.NewTab()
				}},
				mygo.Separator(),
				{Role: mygo.RoleClose},
			},
		},
		{
			Label: "Edit",
			Submenu: []*mygo.MenuItem{
				{Role: mygo.RoleUndo},
				{Role: mygo.RoleRedo},
				mygo.Separator(),
				{Role: mygo.RoleCut},
				{Role: mygo.RoleCopy},
				{Role: mygo.RolePaste},
				{Role: mygo.RoleSelectAll},
			},
		},
		{
			Label: "View",
			Submenu: []*mygo.MenuItem{
				{Label: "Command Palette…", Accelerator: "CmdOrCtrl+Shift+P", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.OpenPalette()
				}},
				mygo.Separator(),
				{Label: "Split Right", Accelerator: "CmdOrCtrl+D", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.SplitRight()
				}},
				{Label: "Split Down", Accelerator: "CmdOrCtrl+Shift+D", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.SplitDown()
				}},
				{Label: "Close Pane", Accelerator: "CmdOrCtrl+Shift+W", Click: func(*mygo.MenuItem, *mygo.Window) {
					application.CloseActivePane()
				}},
				mygo.Separator(),
				{Role: mygo.RoleToggleFullScreen},
				{Role: mygo.RoleZoomIn},
				{Role: mygo.RoleZoomOut},
				{Role: mygo.RoleResetZoom},
			},
		},
		{
			Label: "Window",
			Submenu: []*mygo.MenuItem{
				{Role: mygo.RoleMinimize},
				{Role: mygo.RoleZoom},
				mygo.Separator(),
				{Role: mygo.RoleWindow},
			},
		},
	})
}
