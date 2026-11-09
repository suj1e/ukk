package app

import (
	"context"
	"time"

	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/agents"
	"github.com/suj1e/ukk/apps/desktop/internal/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/conversation"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	uiviews "github.com/suj1e/ukk/apps/desktop/internal/ui/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/terminal"
	uispace "github.com/suj1e/ukk/apps/desktop/internal/ui/workspace"
	domain "github.com/suj1e/ukk/apps/desktop/internal/workspace"
)

type pane struct {
	surface         string
	agentRunID      string
	terminalAddress string
	term            *terminal.State
	files           *uiviews.FilesState
	diff            *uiviews.DiffState

	attachStarted bool
	filesLoaded   bool
	filesLoading  bool
	diffLoaded    bool
	diffLoading   bool
	bindOpen      bool
	err           string
}

func (a *App) paneFor(id string) *pane {
	if id == "" {
		return nil
	}
	leaf := a.tabRoot().Find(id)
	if leaf == nil {
		return nil
	}
	if existing, ok := a.panes[id]; ok {
		return existing
	}
	p := &pane{surface: leaf.Surface, agentRunID: leaf.AgentRunID, terminalAddress: leaf.TerminalAddress}
	a.panes[id] = p
	return p
}

func (a *App) renderPanes(c *ui.Context) {
	root := a.tabRoot()
	if root == nil {
		kit.Empty(c, "No tab", "Create a tab to begin.", "")
		return
	}
	uispace.Layout(c, root, a.sizes, func(nodeID string, percent float32) {
		a.mutateLayout(func(layout *domain.SessionLayout) error {
			for index := range layout.Tabs {
				if layout.Tabs[index].ID != a.activeTab {
					continue
				}
				if node := layout.Tabs[index].Root.Find(nodeID); node != nil {
					node.Ratio = percent / 100
				}
			}
			return nil
		})
	}, func(c *ui.Context, leaf *domain.PaneNode, grow float32) {
		a.renderLeaf(c, leaf)
	})
}

func (a *App) renderLeaf(c *ui.Context, leaf *domain.PaneNode) {
	p := a.paneFor(leaf.ID)
	if p == nil {
		return
	}
	theme := c.Theme()
	active := leaf.ID == a.activePane
	ui.Row(c).Fill().Padding(2).Children(func() {
		rail := ui.Box(c).Width(2).FillHeight()
		if active {
			rail.Background(theme.Accent)
		}
		if rail.Clicked() {
			a.activePane = leaf.ID
		}
		ui.Column(c).Grow(1).FillHeight().Background(theme.Surface).Children(func() {
			a.renderSurface(c, leaf, p)
		})
	})
}

func (a *App) renderSurface(c *ui.Context, leaf *domain.PaneNode, p *pane) {
	switch p.surface {
	case domain.SurfaceChat:
		a.renderConversation(c, p)
	case domain.SurfaceTerm:
		a.renderTerminal(c, leaf, p)
	case domain.SurfaceFiles:
		a.renderFiles(c, p)
	case domain.SurfaceDiff:
		a.renderDiff(c, p)
	case domain.SurfaceLog:
		a.renderActivity(c, p)
	case domain.SurfaceSpec:
		a.renderScreen(c, p)
	default:
		kit.Empty(c, "Unknown surface", p.surface, "")
	}
}

func (a *App) renderConversation(c *ui.Context, p *pane) {
	if a.sessionID == "" {
		kit.Empty(c, "No Session selected", "Create or select a Session first.", "")
		return
	}
	state := a.conversationFor(a.sessionID)
	if state.Snapshot.Provider == "" && !state.Loading && state.ActionErr == "" {
		a.loadAgentSnapshot(state)
	}
	conversation.View(c, state, conversation.Actions{
		Start:  a.startAgent,
		Prompt: a.prompt,
		Abort:  a.abortAgent,
		Stop:   a.stopAgent,
		Approve: func(id string, decision agents.Decision) {
			a.approve(id, string(decision))
		},
	})
}

// loadAgentSnapshot asks the bridge for the current state once per session.
func (a *App) loadAgentSnapshot(state *conversation.State) {
	if a.bridge == nil || state.Loading {
		return
	}
	state.Loading = true
	client := a.bridge
	sessionID := state.SessionID
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 10*time.Second)
		defer cancel()
		snapshot, err := client.AgentSnapshot(ctx, sessionID)
		a.update(func() {
			state.Loading = false
			if err != nil {
				state.ActionErr = err.Error()
				return
			}
			state.Snapshot = toAgentSnapshot(snapshot)
			state.ActionErr = ""
		})
	}()
}

func (a *App) renderTerminal(c *ui.Context, leaf *domain.PaneNode, p *pane) {
	theme := c.Theme()
	if p.term == nil && p.agentRunID != "" {
		if run, ok := a.agentRun(p.agentRunID); ok {
			p.term = terminal.New(run.TerminalAddress, a.agentLabel(run), false)
		}
	}
	if p.term == nil && p.terminalAddress != "" {
		p.term = terminal.New(p.terminalAddress, p.terminalAddress, false)
	}
	if p.term != nil {
		if a.termLibErr != "" {
			kit.Banner(c, "danger", "Terminal engine unavailable: "+a.termLibErr, "")
		} else if !a.termLibReady {
			kit.Banner(c, "warn", "Loading the terminal engine…", "")
		}
		if a.hc == nil && !a.termLoading && !a.termTried && a.hcErr == "" {
			a.refreshTerminals()
		}
		if a.hc != nil && a.termLibReady {
			a.attachPaneTerminal(p)
		}
		uispace.PaneHeader(c, "Terminal", p.term.Address, func() {
			kit.ToolbarButton(c, "Detach").OnClick(func() {
				p.term.Close()
				p.term = nil
				p.attachStarted = false
			})
		})
		terminal.View(c, p.term)
		return
	}

	uispace.PaneHeader(c, "Terminal", "herdr-core", func() {
		kit.ToolbarButton(c, "Refresh").OnClick(a.refreshTerminals)
	})
	if a.hcErr != "" {
		if kit.Banner(c, "danger", a.hcErr, "Retry") {
			a.hcErr = ""
			a.refreshTerminals()
		}
	}
	if a.hc == nil {
		if ui.PrimaryButton(c, "Connect to herdr-core").Clicked() {
			a.termTried = false
			a.refreshTerminals()
		}
		kit.Meta(c, "Connecting reads the terminal list; it never starts or stops a program.")
		return
	}

	bound := a.agents()
	if len(bound) > 0 {
		kit.SectionLabel(c, "Bound to this Session")
		ui.Column(c).Gap(4).Padding(8).Children(func() {
			for _, run := range bound {
				run := run
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Padding(6, 8).Radius(theme.Radius).
					Background(theme.Surface).Border(1, theme.Border).Children(func() {
					ui.Column(c).Grow(1).Gap(1).Children(func() {
						ui.Text(c, a.agentLabel(run)).FontSize(11).FontWeight(600)
						kit.Mono(c, run.CoreID+" · "+run.TerminalAddress)
					})
					ui.Button(c, "Attach").OnClick(func() { a.attachAgentRun(run) })
				})
			}
		})
	}

	kit.SectionLabel(c, "Live terminals")
	rows := a.sortedTerminals()
	if len(rows) == 0 {
		kit.Meta(c, "herdr-core reports no live terminal.")
		return
	}
	ui.Scroll(c).Grow(1).Children(func() {
		ui.Column(c).Gap(4).Padding(8).Children(func() {
			for _, item := range rows {
				item := item
				ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Padding(6, 8).Radius(theme.Radius).
					Background(theme.Surface).Border(1, theme.Border).Children(func() {
					ui.Column(c).Grow(1).Gap(1).Children(func() {
						ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
							ui.Text(c, a.terminalLabel(item)).FontSize(11).FontWeight(600).NoWrap()
							kit.Pill(c, item.State, item.State)
						})
						kit.Mono(c, item.Address)
					})
					ui.Button(c, "Bind").OnClick(func() { a.bindTerminal(item.Address) })
				})
			}
		})
	})
}

func (a *App) renderFiles(c *ui.Context, p *pane) {
	if a.sessionID == "" {
		kit.Empty(c, "No Session selected", "Files belong to a Session's project.", "")
		return
	}
	target, message := a.currentRepo()
	if message != "" {
		kit.Empty(c, "Repository unavailable", message, "")
		return
	}
	if p.files == nil {
		p.files = uiviews.NewFiles(target)
	}
	if !p.filesLoaded && !p.filesLoading {
		a.loadFiles(p)
	}
	uiviews.View(c, p.files, func() {
		if !p.filesLoading {
			a.filesReload(p)
		}
	}, func() {
		a.openDiffPane()
	}, func(path string) {
		a.loadFileInto(p, path)
	})
}

func (a *App) loadFiles(p *pane) {
	p.filesLoading = true
	repo := p.files.Repo
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		status, statusErr := repo.Status(ctx)
		nodes, treeErr := repo.Tree(ctx)
		a.update(func() {
			p.filesLoading = false
			p.filesLoaded = true
			if statusErr != nil {
				p.files.Err = statusErr.Error()
			} else {
				p.files.Status = status
			}
			if treeErr != nil {
				p.files.Err = treeErr.Error()
			} else {
				p.files.Nodes = nodes
			}
		})
	}()
}

func (a *App) filesReload(p *pane) {
	p.filesLoaded = false
	a.loadFiles(p)
}

func (a *App) loadFileInto(p *pane, path string) {
	if p.files == nil || path == "" {
		return
	}
	repo := p.files.Repo
	go func() {
		content, err := repo.Read(path)
		a.update(func() { p.files.SetContent(path, content, err) })
	}()
}

func (a *App) renderDiff(c *ui.Context, p *pane) {
	target, message := a.currentRepo()
	if message != "" {
		kit.Empty(c, "Repository unavailable", message, "")
		return
	}
	if p.diff == nil {
		p.diff = &uiviews.DiffState{}
	}
	if !p.diffLoaded && !p.diffLoading {
		a.loadDiff(p, target)
	}
	uiviews.DiffView(c, p.diff, func() {
		if !p.diffLoading {
			p.diffLoaded = false
			a.loadDiff(p, target)
		}
	})
}

func (a *App) loadDiff(p *pane, target *repository.Repo) {
	p.diffLoading = true
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		patch, err := target.Diff(ctx, repository.ScopeHead)
		a.update(func() {
			p.diffLoading = false
			p.diffLoaded = true
			if err != nil {
				p.diff.Err = err.Error()
				return
			}
			p.diff.SetPatch(patch)
		})
	}()
}

func (a *App) renderActivity(c *ui.Context, p *pane) {
	state := a.conversationFor(a.sessionID)
	uispace.PaneHeader(c, "Activity", "provider events", nil)
	if len(state.Snapshot.Tools) == 0 && len(state.Snapshot.Messages) == 0 {
		kit.Empty(c, "No activity yet", "Provider-reported turns, tools and approvals appear here.", "")
		return
	}
	theme := c.Theme()
	ui.Scroll(c).Grow(1).Children(func() {
		ui.Column(c).Gap(6).Padding(10).Children(func() {
			for _, tool := range state.Snapshot.Tools {
				tool := tool
				ui.Row(c).Gap(8).Children(func() {
					ui.Text(c, tool.Name).FontSize(11).FontWeight(600).Width(160)
					kit.Meta(c, string(tool.Status))
					if tool.IsError {
						kit.Pill(c, "error", "danger")
					}
				})
			}
			for _, approval := range state.Snapshot.PendingApprovals {
				ui.Row(c).Gap(8).Children(func() {
					kit.Pill(c, "approval", "warn")
					ui.Text(c, approval.ToolName).FontSize(11).TextColor(theme.Text)
				})
			}
		})
	})
}

func (a *App) renderScreen(c *ui.Context, p *pane) {
	theme := c.Theme()
	uispace.PaneHeader(c, "Remote Screen", "on demand", nil)
	ui.Column(c).Padding(14).Gap(8).Children(func() {
		ui.Text(c, "Remote Screen is opened from a paired device").
			FontSize(12).FontWeight(600)
		ui.Text(c, "This Session tile controls an active share. Viewing and control are separate grants; capture stops when the viewer stops or the grant is revoked.").
			FontSize(11).TextColor(theme.TextMuted).Wrap()
		if a.bridge == nil {
			kit.Banner(c, "warn", "The bridge is unavailable.", "")
		} else if ui.Button(c, "Stop sharing in this Session").Clicked() {
			a.stopScreen()
		}
		kit.ErrorLine(c, a.settings.ScreenMsg)
	})
}

func (a *App) stopScreen() {
	if a.bridge == nil || a.sessionID == "" {
		return
	}
	client := a.bridge
	sessionID := a.sessionID
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		err := client.ScreenStop(ctx, sessionID)
		a.update(func() {
			if err != nil {
				a.settings.ScreenMsg = err.Error()
			} else {
				a.settings.ScreenMsg = "Screen sharing stopped."
			}
		})
	}()
}

// addTab appends a tab with one surface.
func (a *App) addTab(surface string) {
	a.openTab(domain.Tab{ID: domain.NewID(), Title: surfaceTitle(surface), Root: domain.NewLeaf(surface)})
}

// addTerminalTabTo opens a tab for a core-owned terminal in sessionID and
// shows it when that Session is still the selected one.
func (a *App) addTerminalTabTo(sessionID, address string) {
	layout, ok := a.store.Layout(sessionID)
	if !ok {
		return
	}
	root := domain.NewTerminalLeaf(address)
	tab := domain.Tab{ID: domain.NewID(), Title: "Terminal", Root: root}
	layout.Tabs = append(layout.Tabs, tab)
	layout.ActiveTab = tab.ID
	saved, err := a.store.SaveLayout(sessionID, layout, layout.Revision)
	if err != nil {
		a.notice = err.Error()
		return
	}
	if a.sessionID == sessionID {
		a.layout = saved
		a.activeTab = tab.ID
		a.activePane = root.ID
	}
}

func (a *App) openTab(tab domain.Tab) {
	a.mutateLayout(func(layout *domain.SessionLayout) error {
		layout.Tabs = append(layout.Tabs, tab)
		layout.ActiveTab = tab.ID
		return nil
	})
	a.activeTab = tab.ID
	a.activePane = tab.Root.ID
}

func (a *App) closeTab(id string) {
	if len(a.layout.Tabs) <= 1 {
		return
	}
	a.mutateLayout(func(layout *domain.SessionLayout) error {
		tabs := layout.Tabs[:0]
		for _, tab := range layout.Tabs {
			if tab.ID != id {
				tabs = append(tabs, tab)
			}
		}
		layout.Tabs = tabs
		if layout.ActiveTab == id && len(tabs) > 0 {
			layout.ActiveTab = tabs[0].ID
		}
		return nil
	})
	a.activeTab = a.layout.ActiveTab
	if leaves := a.tabRoot().Leaves(); len(leaves) > 0 {
		a.activePane = leaves[0].ID
	}
}

func (a *App) splitPane(direction, surface string) {
	if a.activePane == "" {
		return
	}
	active := a.activePane
	var created string
	a.mutateLayout(func(layout *domain.SessionLayout) error {
		for index := range layout.Tabs {
			if layout.Tabs[index].ID != a.activeTab {
				continue
			}
			root, err := layout.Tabs[index].Root.SplitLeaf(active, direction, 0.5, surface)
			if err != nil {
				return err
			}
			layout.Tabs[index].Root = root
			if leaves := root.Leaves(); len(leaves) > 0 {
				created = leaves[len(leaves)-1].ID
			}
			return nil
		}
		return domain.ErrNotFound
	})
	if created != "" {
		a.activePane = created
	}
}

func (a *App) closePane(id string) {
	a.mutateLayout(func(layout *domain.SessionLayout) error {
		for index := range layout.Tabs {
			if layout.Tabs[index].ID != a.activeTab {
				continue
			}
			root, err := layout.Tabs[index].Root.CloseLeaf(id)
			if err != nil {
				return err
			}
			layout.Tabs[index].Root = root
			return nil
		}
		return domain.ErrNotFound
	})
	if leaves := a.tabRoot().Leaves(); len(leaves) > 0 {
		a.activePane = leaves[0].ID
	}
}

func (a *App) openDiffPane() {
	a.splitPane(domain.SplitColumn, domain.SurfaceDiff)
}

func surfaceTitle(surface string) string {
	switch surface {
	case domain.SurfaceChat:
		return "Conversation"
	case domain.SurfaceTerm:
		return "Terminal"
	case domain.SurfaceFiles:
		return "Files"
	case domain.SurfaceDiff:
		return "Diff"
	case domain.SurfaceLog:
		return "Activity"
	case domain.SurfaceSpec:
		return "Screen"
	default:
		return surface
	}
}
