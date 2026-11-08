// Command ukk is the native desktop client. Closing its window detaches views;
// Agent processes live in the background bridge and terminals in herdr-core.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/app"
	domain "github.com/suj1e/ukk/apps/desktop/internal/workspace"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--bridge":
			runBridge()
			return
		case "--version", "-version", "version":
			fmt.Println("ukk 0.1.0")
			return
		}
	}
	mygo.App.SetName("ukk")
	mygo.App.SetVersion("0.1.0")

	var application *app.App
	mygo.App.WhenReady(func() {
		dir := os.Getenv("UKK_DATA_DIR")
		if dir == "" {
			resolved, err := mygo.App.Path(mygo.PathUserData)
			if err != nil {
				report("cannot resolve the user data directory", err)
				return
			}
			dir = resolved
		}
		store, err := domain.Open(filepath.Join(dir, "workspace"))
		if err != nil {
			report("cannot open the workspace", err)
			return
		}
		application = app.New(store, dir)
		window := mygo.NewWindow(mygo.WindowOptions{
			Title:     "ukk",
			Width:     1320,
			Height:    860,
			MinWidth:  960,
			MinHeight: 620,
			StateKey:  "main",
			Content:   ui.View(application.View),
		})
		application.Attach(window)
		window.OnClose(func(*mygo.CloseEvent) {
			application.Shutdown()
		})
		window.OnClosed(func() {
			_ = store.Close()
		})
		mygo.App.SetMenu(buildMenu(application))
	})
	if err := mygo.App.Run(); err != nil {
		report("the application stopped", err)
	}
}

func runBridge() {
	if err := bridge.Run(context.Background()); err != nil {
		os.Stderr.WriteString("ukk bridge: " + err.Error() + "\n")
		os.Exit(1)
	}
}

func report(action string, err error) {
	os.Stderr.WriteString("ukk: " + action + ": " + err.Error() + "\n")
}
