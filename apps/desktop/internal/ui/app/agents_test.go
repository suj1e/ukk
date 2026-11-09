package app

import (
	"testing"
	"time"

	"github.com/suj1e/ukk/apps/desktop/internal/hc"
)

func TestTerminalAgentsReadOSCStatus(t *testing.T) {
	reported := uint64(time.Now().Add(-90 * time.Second).UnixMilli())
	name := "shell"
	app := &App{terminals: []hc.Terminal{
		{
			Address: "t6", State: "live", Title: "TREE: contracts",
			Status:   &hc.Status{State: "working", App: "omp"},
			StatusMS: &reported,
			Labels:   map[string]string{"where": "ukk / 1"},
		},
		{
			Address: "t1", State: "live",
			Status: &hc.Status{State: "done", App: "omp"},
		},
		{
			Address: "t3", State: "live", Name: &name,
		},
	}}

	agents := app.terminalAgents()
	if len(agents) != 2 {
		t.Fatalf("agents = %d, want 2 (a terminal without status is not an Agent)", len(agents))
	}
	first := agents[0]
	if first.Group != "Working" || first.App != "omp" || first.Title != "TREE: contracts" {
		t.Fatalf("first agent = %+v", first)
	}
	if first.Place != "ukk / 1" {
		t.Fatalf("place = %q", first.Place)
	}
	if first.Elapsed == "" {
		t.Fatal("working agent has no elapsed time")
	}
	if agents[1].Group != "Idle" || agents[1].Title != "t1" {
		t.Fatalf("second agent = %+v", agents[1])
	}
}

func TestAgentGroup(t *testing.T) {
	cases := map[string]string{
		"working": "Working",
		"blocked": "Needs you",
		"error":   "Needs you",
		"done":    "Idle",
		"idle":    "Idle",
		"strange": "Agents",
	}
	for state, want := range cases {
		if got := agentGroup(state); got != want {
			t.Errorf("agentGroup(%q) = %q, want %q", state, got, want)
		}
	}
}
