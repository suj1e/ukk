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
	byAddress := map[string]terminalAgent{}
	for _, agent := range agents {
		byAddress[agent.Terminal.Address] = agent
	}
	working := byAddress["t6"]
	if working.Group != "Working" || working.App != "omp" || working.Title != "TREE: contracts" {
		t.Fatalf("working agent = %+v", working)
	}
	if working.Place != "ukk / 1" {
		t.Fatalf("place = %q", working.Place)
	}
	if working.Elapsed == "" {
		t.Fatal("working agent has no elapsed time")
	}
	done := byAddress["t1"]
	if done.Group != "Done" || done.Title != "t1" {
		t.Fatalf("done agent = %+v", done)
	}
	// Herdr ranks Needs you, Done, Working, Idle.
	if agents[0].Group != "Done" || agents[1].Group != "Working" {
		t.Fatalf("order = %q, %q", agents[0].Group, agents[1].Group)
	}
}

func TestAgentGroup(t *testing.T) {
	cases := map[string]string{
		"working": "Working",
		"blocked": "Needs you",
		"error":   "Needs you",
		"done":    "Done",
		"idle":    "Idle",
		"strange": "Agents",
	}
	for state, want := range cases {
		if got := agentGroup(state); got != want {
			t.Errorf("agentGroup(%q) = %q, want %q", state, got, want)
		}
	}
}
