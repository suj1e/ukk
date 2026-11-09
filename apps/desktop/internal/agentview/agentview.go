// Package agentview projects herdr-core terminal facts into the sidebar's
// agent rows. The projection mirrors the official Herdr agents mod: grouped
// sections ordered by attention (Needs you, Done, Working, Idle, Agents),
// each row carrying the program's reported state, a cleaned title, where the
// agent works and how long its current state has lasted.
//
// It is pure data: the package imports only hc and the standard library, reads
// no terminal text to infer state, and never touches UI, bridge or workspace
// packages.
package agentview

import (
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/suj1e/ukk/apps/desktop/internal/hc"
)

// Section headings and their attention ranks, highest first. A state outside
// the known set stays in Agents; the package never guesses a state from text.
const (
	groupNeedsYou = "Needs you"
	groupDone     = "Done"
	groupWorking  = "Working"
	groupIdle     = "Idle"
	groupAgents   = "Agents"

	rankNeedsYou = 4
	rankDone     = 3
	rankWorking  = 2
	rankIdle     = 1
	rankAgents   = 0
)

// Agent is one sidebar row projected from a herdr-core terminal report.
type Agent struct {
	// Address is the terminal's herdr-core address.
	Address string
	// App is the program's OSC 7501 app word, or empty when it reported none.
	App string
	// State is the program's OSC 7501 state word verbatim.
	State string
	// Group is the sidebar section heading this row belongs to: "Needs you",
	// "Done", "Working", "Idle" or "Agents".
	Group string
	// Title is the terminal's live title with leading non-letter/number
	// spinner glyphs stripped, falling back to the terminal name, raw title,
	// foreground process name and finally the address.
	Title string
	// Place is the terminal's labels["where"], or its working directory.
	Place string
	// Rank orders Group: 4 Needs you, 3 Done, 2 Working, 1 Idle, 0 Agents.
	Rank int
	// SinceMS is when the reported state last changed: status_ms, or created_ms
	// when the program has not reported one.
	SinceMS uint64
	// Elapsed renders now-SinceMS as "<1m", "Xm", "Xh" or "Xd".
	Elapsed string
	// Seen reports whether the shared herdr.seen label has reached Turn. It is
	// false when no turn has happened or the label is missing or malformed.
	Seen bool
	// Turn is core's turn counter: it rises each time the program stops working.
	Turn uint64
}

// Section is one sidebar heading and the rows under it.
type Section struct {
	// Name is the heading: "Needs you", "Done", "Working", "Idle" or "Agents".
	Name string
	// Rank is the heading's attention order, 4 through 0.
	Rank int
	// Agents are the section's rows, most recently changed first.
	Agents []Agent
}

// FromTerminals projects terminal reports into sidebar rows. Only terminals
// that carry a non-empty status state become rows; everything else is skipped.
// now is injected so durations are deterministic.
func FromTerminals(terminals []hc.Terminal, now time.Time) []Agent {
	agents := make([]Agent, 0, len(terminals))
	for _, term := range terminals {
		if term.Status == nil || term.Status.State == "" {
			continue
		}
		group, rank := sectionOf(term.Status.State)
		since := stateSince(term)
		agents = append(agents, Agent{
			Address: term.Address,
			App:     term.Status.App,
			State:   term.Status.State,
			Group:   group,
			Title:   displayTitle(term),
			Place:   placeOf(term),
			Rank:    rank,
			SinceMS: since,
			Elapsed: formatElapsed(ageMS(now, since)),
			Seen:    seenAt(term.Labels, term.Turn),
			Turn:    term.Turn,
		})
	}
	return agents
}

// Sections groups rows into headings ordered by rank, highest first. Rows
// within a heading are ordered by SinceMS descending, then title, then address;
// headings without rows are omitted.
func Sections(agents []Agent) []Section {
	byName := make(map[string]*Section)
	order := make([]*Section, 0, len(agents))
	for _, agent := range agents {
		section, ok := byName[agent.Group]
		if !ok {
			section = &Section{Name: agent.Group, Rank: agent.Rank}
			byName[agent.Group] = section
			order = append(order, section)
		}
		section.Agents = append(section.Agents, agent)
	}
	for _, section := range order {
		sort.Slice(section.Agents, func(i, j int) bool {
			a, b := section.Agents[i], section.Agents[j]
			if a.SinceMS != b.SinceMS {
				return a.SinceMS > b.SinceMS
			}
			if a.Title != b.Title {
				return a.Title < b.Title
			}
			return a.Address < b.Address
		})
	}
	sort.Slice(order, func(i, j int) bool {
		if order[i].Rank != order[j].Rank {
			return order[i].Rank > order[j].Rank
		}
		return order[i].Name < order[j].Name
	})
	if len(order) == 0 {
		return nil
	}
	sections := make([]Section, len(order))
	for i, section := range order {
		sections[i] = *section
	}
	return sections
}

// sectionOf maps a reported state to its sidebar heading and rank. The mapping
// is total: unknown states are Agents, never an inferred activity.
func sectionOf(state string) (string, int) {
	switch state {
	case "blocked", "error":
		return groupNeedsYou, rankNeedsYou
	case "done":
		return groupDone, rankDone
	case "working":
		return groupWorking, rankWorking
	case "idle":
		return groupIdle, rankIdle
	default:
		return groupAgents, rankAgents
	}
}

// displayTitle follows the official sidebar's fallback chain: the cleaned live
// title, the terminal name, the raw title, the foreground process name, then
// the address.
func displayTitle(term hc.Terminal) string {
	if title := cleanTitle(term.Title); title != "" {
		return title
	}
	if term.Name != nil && *term.Name != "" {
		return *term.Name
	}
	if term.Title != "" {
		return term.Title
	}
	if term.Foreground != nil && term.Foreground.Name != "" {
		return term.Foreground.Name
	}
	return term.Address
}

// cleanTitle drops the spinner and status glyphs a program puts before its
// title, as Herdr's agents mod does. A title with no letter or number left is
// empty, so the caller can fall back.
func cleanTitle(raw string) string {
	for i, r := range raw {
		if unicode.IsLetter(r) || unicode.IsNumber(r) {
			return strings.TrimSpace(raw[i:])
		}
	}
	return ""
}

// placeOf is where the agent works: the shared "where" label when present,
// otherwise the terminal's current directory.
func placeOf(term hc.Terminal) string {
	if where := term.Labels["where"]; where != "" {
		return where
	}
	return term.Cwd
}

// stateSince is when the reported state last changed. A terminal that never
// reported a status change falls back to its creation time.
func stateSince(term hc.Terminal) uint64 {
	if term.StatusMS != nil {
		return *term.StatusMS
	}
	return term.CreatedMS
}

// seenAt compares the shared herdr.seen label with core's turn counter. Turns
// before the first stop carry no attention, and a malformed label is unseen.
func seenAt(labels map[string]string, turn uint64) bool {
	if turn == 0 {
		return false
	}
	raw, ok := labels["herdr.seen"]
	if !ok {
		return false
	}
	seen, err := strconv.ParseUint(strings.TrimSpace(raw), 10, 64)
	if err != nil {
		return false
	}
	return seen >= turn
}

// ageMS is now-since in milliseconds, clamped at zero so a clock that disagrees
// with core still reads as fresh.
func ageMS(now time.Time, since uint64) int64 {
	nowMS := now.UnixMilli()
	if nowMS <= 0 || since >= uint64(nowMS) {
		return 0
	}
	return nowMS - int64(since)
}

// formatElapsed is Herdr's compact age: "<1m", "Xm", "Xh", "Xd".
func formatElapsed(ms int64) string {
	minutes := ms / 60000
	if minutes < 1 {
		return "<1m"
	}
	if minutes < 60 {
		return strconv.FormatInt(minutes, 10) + "m"
	}
	hours := minutes / 60
	if hours < 24 {
		return strconv.FormatInt(hours, 10) + "h"
	}
	return strconv.FormatInt(hours/24, 10) + "d"
}
