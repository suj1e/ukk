// Package conversation renders provider-neutral Agent messages, tool calls and
// approvals. It never infers structure from terminal output.
package conversation

import (
	"fmt"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
	"github.com/suj1e/ukk/apps/desktop/internal/agents"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
)

// Actions are the user-intent callbacks of the surface. A nil callback hides
// the action rather than faking a result.
type Actions struct {
	Start   func()
	Prompt  func(text string)
	Abort   func()
	Stop    func()
	Approve func(id string, decision agents.Decision)
}

// State is one conversation pane's presentation state.
type State struct {
	SessionID string
	Snapshot  agents.Snapshot
	Loading   bool
	Draft     string
	ActionErr string
	Stopping  bool
	Expanded  map[string]bool
	FullText  map[string]bool
	List      ui.ListState
}

// New returns an empty conversation state.
func New(sessionID string) *State {
	return &State{SessionID: sessionID, Expanded: map[string]bool{}, FullText: map[string]bool{}}
}

type rowKind int

const (
	rowMessage rowKind = iota
	rowTool
	rowApproval
)

type row struct {
	kind rowKind
	at   int64
	msg  agents.Message
	tool agents.ToolCall
	appr agents.Approval
}

func rowsOf(s agents.Snapshot) []row {
	rows := make([]row, 0, len(s.Messages)+len(s.Tools)+len(s.PendingApprovals))
	for _, m := range s.Messages {
		rows = append(rows, row{kind: rowMessage, at: m.CreatedMS, msg: m})
	}
	for _, t := range s.Tools {
		rows = append(rows, row{kind: rowTool, at: t.StartedMS, tool: t})
	}
	for _, a := range s.PendingApprovals {
		rows = append(rows, row{kind: rowApproval, at: a.ExpiresMS - 60_000, appr: a})
	}
	sort.SliceStable(rows, func(i, j int) bool { return rows[i].at < rows[j].at })
	return rows
}

// View renders the conversation pane.
func View(c *ui.Context, st *State, act Actions) {
	if st.Snapshot.Provider == "" && !st.Loading {
		started := kit.Empty(c,
			"No Agent running in this Session",
			"Start omp in the Session's project root. This creates a structured Agent session, not a terminal emulation.",
			"Start Agent")
		if started && act.Start != nil {
			act.Start()
		}
		kit.ErrorLine(c, st.ActionErr)
		return
	}

	theme := c.Theme()
	status := st.Snapshot.State
	if status == "" {
		status = agents.StateUnknown
	}
	ui.Row(c).FillWidth().Height(34).PaddingX(12).Gap(8).AlignItems(ui.Center).
		BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).Children(func() {
		ui.Box(c).Size(7, 7).Radius(999).Background(stateColor(theme, status))
		ui.Text(c, st.Snapshot.Provider).FontSize(11).FontWeight(600).NoWrap()
		kit.Meta(c, string(status))
		if st.Snapshot.ProviderSessionID != "" {
			kit.Mono(c, kit.ShortID(st.Snapshot.ProviderSessionID))
		}
		ui.Spacer(c)
		if st.Snapshot.Usage.TotalTokens > 0 {
			kit.Meta(c, fmt.Sprintf("%d tokens", st.Snapshot.Usage.TotalTokens))
		}
		switch {
		case st.Loading:
			ui.Spinner(c).Size(14, 14)
		case status == agents.StateRunning || status == agents.StateBlocked:
			if act.Abort != nil {
				kit.ToolbarButton(c, "Cancel turn").OnClick(act.Abort)
			}
		case act.Stop != nil:
			if st.Stopping {
				kit.ToolbarButton(c, "Confirm stop").OnClick(act.Stop)
				kit.ToolbarButton(c, "Keep running").OnClick(func() { st.Stopping = false })
			} else {
				kit.ToolbarButton(c, "Stop Agent").OnClick(func() { st.Stopping = true })
			}
		}
	})
	kit.ErrorLine(c, st.ActionErr)
	if st.Snapshot.Error != nil {
		kit.ErrorLine(c, st.Snapshot.Error.Message)
	}

	rows := rowsOf(st.Snapshot)
	st.List.Key = func(i int) any {
		r := rows[i]
		switch r.kind {
		case rowMessage:
			return "m:" + r.msg.ID
		case rowTool:
			return "t:" + r.tool.ID
		default:
			return "a:" + r.appr.ID
		}
	}
	st.List.FollowEnd = status == agents.StateRunning
	st.List.FollowAnimation = &ui.Smooth

	if len(rows) == 0 {
		ui.Column(c).Grow(1).Center().Padding(24).Children(func() {
			kit.Meta(c, "The Agent has not produced a turn yet.")
		})
	} else {
		ui.Column(c).Selectable().Grow(1).Children(func() {
			ui.List(c, &st.List, len(rows), func(i int) {
				r := rows[i]
				switch r.kind {
				case rowMessage:
					message(c, st, r.msg)
				case rowTool:
					tool(c, st, r.tool)
				case rowApproval:
					approval(c, r.appr, act)
				}
			}).Grow(1).Justify(ui.End)
		})
	}
	composer(c, st, act)
}

func message(c *ui.Context, st *State, m agents.Message) {
	theme := c.Theme()
	c.Root()
	switch m.Role {
	case agents.RoleUser:
		ui.Column(c).FillWidth().Padding(8, 10).MaxWidthPercent(88).Radius(theme.Radius).
			Margin(10, 14, 4, 14).Background(theme.Surface).Border(1, theme.Border).Children(func() {
			ui.Text(c, m.Text).FontSize(12).Wrap()
		})
	case agents.RoleSystem:
		ui.Row(c).FillWidth().Gap(6).PaddingY(6).PaddingX(14).Children(func() {
			kit.Pill(c, "system", "")
			ui.Text(c, m.Text).FontSize(11).TextColor(theme.TextMuted).Wrap().Grow(1)
		})
	default:
		ui.Column(c).PaddingY(8).PaddingX(14).Children(func() {
			opts := &markdown.Options{
				ShareSelection: true,
				OnLink: func(url string) {
					if ui.SafeToOpen(url) {
						c.OpenURL(url)
					}
				},
			}
			text := m.Text
			limit := 64 << 10
			if st.FullText[m.ID] {
				limit = 2 << 20
			}
			truncated := len(text) > limit
			if truncated {
				text = text[:limit]
				if index := strings.LastIndexByte(text, '\n'); index > limit/2 {
					text = text[:index]
				} else {
					for len(text) > 0 && !utf8.ValidString(text) {
						text = text[:len(text)-1]
					}
				}
			}
			for text != "" {
				part := text
				if len(part) > 64<<10 {
					cut := 64 << 10
					if index := strings.LastIndexByte(part[:cut], '\n'); index > cut/2 {
						cut = index
					}
					part = text[:cut]
				}
				text = text[len(part):]
				if m.Final || text != "" {
					markdown.View(c, part, opts)
				} else {
					markdown.Stream(c, part, opts)
				}
			}
			if truncated {
				ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
					kit.Meta(c, fmt.Sprintf("Showing %d KiB of %d KiB.", limit>>10, len(m.Text)>>10))
					if !st.FullText[m.ID] {
						ui.Button(c, "Show more").OnClick(func() { st.FullText[m.ID] = true })
					}
				})
			}
		})
	}
}

func tool(c *ui.Context, st *State, t agents.ToolCall) {
	theme := c.Theme()
	color := theme.TextMuted
	switch t.Status {
	case agents.ToolRunning:
		color = theme.Accent
	case agents.ToolCompleted:
		color = theme.Success
	case agents.ToolFailed:
		color = theme.Danger
	}
	expanded := st.Expanded[t.ID]
	card := ui.Column(c).FillWidth().Gap(4).Padding(7, 9).Radius(theme.Radius).
		Margin(4, 14, 4, 14).Background(theme.Surface).Border(1, theme.Border)
	card.Children(func() {
		head := ui.ButtonBase(c).FillWidth()
		head.Label("Tool " + t.Name)
		head.Children(func() {
			ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
				ui.Box(c).Size(6, 6).Radius(999).Background(color)
				ui.Text(c, t.Name).FontSize(11).FontWeight(600).NoWrap()
				if t.Status != "" {
					kit.Meta(c, string(t.Status))
				}
				if d := t.EndedMS - t.StartedMS; d > 0 {
					kit.Meta(c, kit.FormatDuration(d))
				}
				ui.Spacer(c)
				kit.Meta(c, map[bool]string{true: "Hide", false: "Details"}[expanded])
			})
		})
		if head.Clicked() {
			st.Expanded[t.ID] = !expanded
		}
		if expanded {
			if len(t.Input) > 0 {
				ui.Text(c, "Input").FontSize(10).TextColor(theme.TextMuted)
				ui.Text(c, clamp(string(t.Input), 4000)).FontSize(11).Font("Menlo, Consolas, monospace").Wrap()
			}
			if len(t.Output) > 0 {
				ui.Text(c, "Output").FontSize(10).TextColor(theme.TextMuted)
				ui.Text(c, clamp(string(t.Output), 4000)).FontSize(11).Font("Menlo, Consolas, monospace").Wrap()
			}
		}
	})
}

func approval(c *ui.Context, a agents.Approval, act Actions) {
	theme := c.Theme()
	ui.Column(c).FillWidth().Gap(6).Padding(9, 11).Radius(theme.Radius).
		Margin(4, 14, 4, 14).Background(theme.Surface).Border(1, theme.Warning).Children(func() {
		ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
			kit.Pill(c, "approval", "warn")
			ui.Text(c, a.ToolName).FontSize(12).FontWeight(600)
		})
		ui.Text(c, "This Agent asks to run the tool above. Approving permits its execution.").
			FontSize(11).TextColor(theme.TextMuted).Wrap()
		if len(a.Input) > 0 {
			ui.Text(c, clamp(string(a.Input), 2000)).FontSize(11).Font("Menlo, Consolas, monospace").Wrap()
		}
		ui.Row(c).Gap(6).Children(func() {
			if act.Approve != nil {
				if ui.PrimaryButton(c, "Approve").Clicked() {
					act.Approve(a.ID, agents.DecisionApprove)
				}
				if ui.Button(c, "Deny").Clicked() {
					act.Approve(a.ID, agents.DecisionDeny)
				}
			} else {
				kit.Meta(c, "Approval is unavailable in this client.")
			}
		})
	})
}

func composer(c *ui.Context, st *State, act Actions) {
	theme := c.Theme()
	running := st.Snapshot.State == agents.StateRunning
	ui.Column(c).FillWidth().Gap(6).Padding(8, 10).
		BorderWidth(1, 0, 0, 0).BorderColor(theme.Border).Children(func() {
		ui.TextArea(c, &st.Draft).Placeholder("Message the Agent").
			Lines(1, 6).MaxLength(64 * 1024).FontSize(12).FillWidth()
		ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
			if running {
				kit.Meta(c, "The Agent is working. The next message queues after the current turn.")
			} else {
				ui.Spacer(c)
			}
			send := func() {
				text := strings.TrimSpace(st.Draft)
				if text == "" || act.Prompt == nil {
					return
				}
				st.Draft = ""
				act.Prompt(text)
			}
			if act.Prompt != nil {
				ui.PrimaryButton(c, "Send").Disabled(strings.TrimSpace(st.Draft) == "").
					OnClick(send).OnShortcut(ui.Cmd, ui.KeyEnter, send)
			}
		})
	})
}

func stateColor(t *ui.Theme, state agents.State) ui.Color {
	switch state {
	case agents.StateRunning:
		return t.Accent
	case agents.StateBlocked:
		return t.Warning
	case agents.StateDone:
		return t.Success
	case agents.StateFailed, agents.StateCancelled:
		return t.Danger
	default:
		return t.TextMuted
	}
}

func clamp(text string, max int) string {
	if len(text) <= max {
		return text
	}
	return text[:max] + "\n…"
}

var _ = time.Now
