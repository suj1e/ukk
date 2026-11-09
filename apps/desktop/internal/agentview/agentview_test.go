package agentview

import (
	"reflect"
	"testing"
	"time"

	"github.com/suj1e/ukk/apps/desktop/internal/hc"
)

func TestFromTerminalsSkipsTerminalsWithoutStatus(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	terminals := []hc.Terminal{
		{Address: "no-status"},
		{Address: "no-state", Status: &hc.Status{App: "claude-code"}},
		{Address: "reported", Status: &hc.Status{State: "working"}},
	}
	got := FromTerminals(terminals, now)
	if len(got) != 1 {
		t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
	}
	if got[0].Address != "reported" {
		t.Fatalf("row address = %q, want reported", got[0].Address)
	}
}

func TestFromTerminalsMapsStateToSection(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	tests := []struct {
		state string
		group string
		rank  int
	}{
		{state: "blocked", group: "Needs you", rank: 4},
		{state: "error", group: "Needs you", rank: 4},
		{state: "done", group: "Done", rank: 3},
		{state: "working", group: "Working", rank: 2},
		{state: "idle", group: "Idle", rank: 1},
		{state: "paused", group: "Agents", rank: 0},
		{state: "ATTENTION", group: "Agents", rank: 0},
	}
	for _, tt := range tests {
		t.Run(tt.state, func(t *testing.T) {
			term := hc.Terminal{
				Address: "t1",
				Status:  &hc.Status{State: tt.state},
			}
			got := FromTerminals([]hc.Terminal{term}, now)
			if len(got) != 1 {
				t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
			}
			if got[0].State != tt.state {
				t.Errorf("State = %q, want %q", got[0].State, tt.state)
			}
			if got[0].Group != tt.group {
				t.Errorf("Group = %q, want %q", got[0].Group, tt.group)
			}
			if got[0].Rank != tt.rank {
				t.Errorf("Rank = %d, want %d", got[0].Rank, tt.rank)
			}
		})
	}
}

func TestFromTerminalsProjectsReportedFacts(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	nowMS := uint64(now.UnixMilli())
	term := hc.Terminal{
		Address:   "box/one",
		Cwd:       "/fallback",
		CreatedMS: nowMS - 3_600_000,
		Labels:    map[string]string{"where": "/repo", "herdr.seen": "7"},
		Status:    &hc.Status{State: "working", App: "claude-code"},
		Title:     "⠋ Fixing tests",
		StatusMS:  new(nowMS - 5*60_000),
		Turn:      6,
		Foreground: &hc.Foreground{
			Name: "bash",
		},
	}
	got := FromTerminals([]hc.Terminal{term}, now)
	if len(got) != 1 {
		t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
	}
	want := Agent{
		Address: "box/one",
		App:     "claude-code",
		State:   "working",
		Group:   "Working",
		Title:   "Fixing tests",
		Place:   "/repo",
		Rank:    2,
		SinceMS: nowMS - 5*60_000,
		Elapsed: "5m",
		Seen:    true,
		Turn:    6,
	}
	if !reflect.DeepEqual(got[0], want) {
		t.Fatalf("row = %+v, want %+v", got[0], want)
	}
}

func TestFromTerminalsTitleFallbacks(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	tests := []struct {
		name       string
		title      string
		terminal   *string
		foreground *hc.Foreground
		address    string
		want       string
	}{
		{name: "spinner stripped", title: "⠋ Building", want: "Building"},
		{name: "glyph before number", title: "✳ 42 tests", want: "42 tests"},
		{name: "plain title trimmed", title: "  plain title  ", want: "plain title"},
		{name: "name before raw glyphs", title: "!!!", terminal: new("named"), want: "named"},
		{name: "raw title when no letters", title: "!!!", want: "!!!"},
		{name: "empty name skipped for foreground", terminal: new(""), foreground: &hc.Foreground{Name: "bash"}, want: "bash"},
		{name: "name wins over foreground", title: "", terminal: new("named"), foreground: &hc.Foreground{Name: "bash"}, want: "named"},
		{name: "address last", title: "", address: "box/one", want: "box/one"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			term := hc.Terminal{
				Address:    tt.address,
				Name:       tt.terminal,
				Title:      tt.title,
				Foreground: tt.foreground,
				Status:     &hc.Status{State: "idle"},
			}
			got := FromTerminals([]hc.Terminal{term}, now)
			if len(got) != 1 {
				t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
			}
			if got[0].Title != tt.want {
				t.Fatalf("Title = %q, want %q", got[0].Title, tt.want)
			}
		})
	}
}

func TestFromTerminalsPlaceFallback(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	tests := []struct {
		name   string
		labels map[string]string
		cwd    string
		want   string
	}{
		{name: "where label", labels: map[string]string{"where": "~/src"}, cwd: "/tmp", want: "~/src"},
		{name: "cwd when label missing", labels: map[string]string{"herdr.seen": "1"}, cwd: "/tmp", want: "/tmp"},
		{name: "cwd when label empty", labels: map[string]string{"where": ""}, cwd: "/tmp", want: "/tmp"},
		{name: "nil labels", cwd: "/tmp", want: "/tmp"},
		{name: "both empty", cwd: "", want: ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			term := hc.Terminal{
				Address: "t1",
				Cwd:     tt.cwd,
				Labels:  tt.labels,
				Status:  &hc.Status{State: "idle"},
			}
			got := FromTerminals([]hc.Terminal{term}, now)
			if len(got) != 1 {
				t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
			}
			if got[0].Place != tt.want {
				t.Fatalf("Place = %q, want %q", got[0].Place, tt.want)
			}
		})
	}
}

func TestFromTerminalsSeenComparesSharedTurn(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	tests := []struct {
		name    string
		turn    uint64
		seen    string
		hasSeen bool
		want    bool
	}{
		{name: "no turn yet", turn: 0, seen: "5", hasSeen: true, want: false},
		{name: "equal turn", turn: 3, seen: "3", hasSeen: true, want: true},
		{name: "beyond turn", turn: 3, seen: "4", hasSeen: true, want: true},
		{name: "behind turn", turn: 3, seen: "2", hasSeen: true, want: false},
		{name: "empty label", turn: 3, seen: "", hasSeen: true, want: false},
		{name: "malformed label", turn: 3, seen: "abc", hasSeen: true, want: false},
		{name: "spaces around value", turn: 3, seen: " 4 ", hasSeen: true, want: true},
		{name: "missing label", turn: 3, hasSeen: false, want: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			labels := map[string]string{}
			if tt.hasSeen {
				labels["herdr.seen"] = tt.seen
			}
			term := hc.Terminal{
				Address: "t1",
				Labels:  labels,
				Status:  &hc.Status{State: "working"},
				Turn:    tt.turn,
			}
			got := FromTerminals([]hc.Terminal{term}, now)
			if len(got) != 1 {
				t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
			}
			if got[0].Seen != tt.want {
				t.Fatalf("Seen = %v, want %v (turn %d, label %q)", got[0].Seen, tt.want, tt.turn, tt.seen)
			}
			if got[0].Turn != tt.turn {
				t.Fatalf("Turn = %d, want %d", got[0].Turn, tt.turn)
			}
		})
	}
}

func TestFromTerminalsElapsedFormats(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	nowMS := uint64(now.UnixMilli())
	tests := []struct {
		name      string
		statusMS  *uint64
		createdMS uint64
		wantSince uint64
		want      string
	}{
		{name: "under a minute", statusMS: new(nowMS - 30_000), wantSince: nowMS - 30_000, want: "<1m"},
		{name: "minutes", statusMS: new(nowMS - 5*60_000), wantSince: nowMS - 5*60_000, want: "5m"},
		{name: "an hour", statusMS: new(nowMS - 90*60_000), wantSince: nowMS - 90*60_000, want: "1h"},
		{name: "hours", statusMS: new(nowMS - 3*3_600_000), wantSince: nowMS - 3*3_600_000, want: "3h"},
		{name: "days", statusMS: new(nowMS - 49*3_600_000), wantSince: nowMS - 49*3_600_000, want: "2d"},
		{name: "created fallback", createdMS: nowMS - 2*3_600_000, wantSince: nowMS - 2*3_600_000, want: "2h"},
		{name: "future clamps", statusMS: new(nowMS + 60_000), wantSince: nowMS + 60_000, want: "<1m"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			term := hc.Terminal{
				Address:   "t1",
				CreatedMS: tt.createdMS,
				Status:    &hc.Status{State: "idle"},
				StatusMS:  tt.statusMS,
			}
			got := FromTerminals([]hc.Terminal{term}, now)
			if len(got) != 1 {
				t.Fatalf("FromTerminals returned %d rows, want 1", len(got))
			}
			if got[0].SinceMS != tt.wantSince {
				t.Fatalf("SinceMS = %d, want %d", got[0].SinceMS, tt.wantSince)
			}
			if got[0].Elapsed != tt.want {
				t.Fatalf("Elapsed = %q, want %q", got[0].Elapsed, tt.want)
			}
		})
	}
}

func TestSectionsOrdersByRankThenRecencyAndTitle(t *testing.T) {
	agents := []Agent{
		{Address: "w2", Group: "Working", Rank: 2, SinceMS: 300, Title: "b"},
		{Address: "i1", Group: "Idle", Rank: 1, SinceMS: 900, Title: "i"},
		{Address: "w1", Group: "Working", Rank: 2, SinceMS: 300, Title: "a"},
		{Address: "n1", Group: "Needs you", Rank: 4, SinceMS: 100, Title: "n"},
		{Address: "w3", Group: "Working", Rank: 2, SinceMS: 100, Title: "z"},
		{Address: "a1", Group: "Agents", Rank: 0, SinceMS: 500, Title: "x"},
		{Address: "d1", Group: "Done", Rank: 3, SinceMS: 200, Title: "d"},
	}
	got := Sections(agents)
	wantNames := []string{"Needs you", "Done", "Working", "Idle", "Agents"}
	wantRanks := []int{4, 3, 2, 1, 0}
	if len(got) != len(wantNames) {
		t.Fatalf("Sections returned %d sections, want %d", len(got), len(wantNames))
	}
	for i, section := range got {
		if section.Name != wantNames[i] || section.Rank != wantRanks[i] {
			t.Fatalf("section %d = %q/%d, want %q/%d", i, section.Name, section.Rank, wantNames[i], wantRanks[i])
		}
	}
	working := got[2].Agents
	var gotWorking []string
	for _, agent := range working {
		gotWorking = append(gotWorking, agent.Address)
	}
	wantWorking := []string{"w1", "w2", "w3"}
	if !reflect.DeepEqual(gotWorking, wantWorking) {
		t.Fatalf("Working agents = %v, want %v", gotWorking, wantWorking)
	}
}

func TestSectionsEmpty(t *testing.T) {
	if got := Sections(nil); got != nil {
		t.Fatalf("Sections(nil) = %+v, want nil", got)
	}
	if got := Sections([]Agent{}); len(got) != 0 {
		t.Fatalf("Sections(empty) has %d sections, want 0", len(got))
	}
}

func TestFromTerminalsUnknownStateStaysInAgents(t *testing.T) {
	now := time.UnixMilli(1_700_000_000_000)
	terminals := []hc.Terminal{
		{Address: "a", Status: &hc.Status{State: "working"}},
		{Address: "b", Status: &hc.Status{State: "teleporting"}},
		{Address: "c", Status: &hc.Status{State: "done"}},
	}
	sections := Sections(FromTerminals(terminals, now))
	if len(sections) != 3 {
		t.Fatalf("Sections returned %d sections, want 3", len(sections))
	}
	if sections[0].Name != "Done" || sections[1].Name != "Working" || sections[2].Name != "Agents" {
		t.Fatalf("section order = %q, %q, %q", sections[0].Name, sections[1].Name, sections[2].Name)
	}
	if sections[2].Agents[0].Address != "b" {
		t.Fatalf("unknown state landed at %q, want b", sections[2].Agents[0].Address)
	}
}
