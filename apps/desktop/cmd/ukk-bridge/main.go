// Command ukk-bridge is the background Agent and remote-access service. It
// outlives Desktop windows; an explicit service stop ends its Agent children.
package main

import (
	"context"
	"fmt"
	"os"

	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "serve":
		case "--version", "-version", "version":
			fmt.Println("ukk-bridge 0.1.0")
			return
		case "-h", "--help", "help":
			fmt.Println("usage: ukk-bridge [serve]\n\nRuns the local Agent bridge and optional relay connection.")
			return
		default:
			fmt.Fprintln(os.Stderr, "ukk-bridge: unknown command", os.Args[1])
			os.Exit(2)
		}
	}
	if err := bridge.Run(context.Background()); err != nil {
		fmt.Fprintln(os.Stderr, "ukk-bridge:", err)
		os.Exit(1)
	}
}
