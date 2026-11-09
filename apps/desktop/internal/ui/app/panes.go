package app

import (
	"context"
	"encoding/json"
	"time"

	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/agents"
	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
	"github.com/suj1e/ukk/apps/desktop/internal/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/conversation"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	uiviews "github.com/suj1e/ukk/apps/desktop/internal/ui/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/terminal"
	"github.com/suj1e/ukk/apps/desktop/internal/world"
)

// pane is the presentation state of one block, keyed by the block's id. It
// holds attachments and view models only; the terminal process lives in
// herdr-core and the Agent lives in the bridge.
type pane struct {
	terminalAddress string
	convSession     string
	folder          string
	term            *terminal.State
	files           *uiviews.FilesState
	diff            *uiviews.DiffState

	attachStarted bool
	filesLoaded   bool
	filesLoading  bool
	diffLoaded    bool
	diffLoading   bool
}

// paneFor returns the presentation state for a block, reconciling it with the
// block's current content. The shared document can be edited by another
// client, so an address or mod props may legitimately change under an id.
func (a *App) paneFor(block corelayout.Node) *pane {
	address := blockTerminalAddress(block)
	sessionID := conversationBlockSession(block)
	if existing, ok := a.panes[block.ID]; ok {
		if existing.terminalAddress != address {
			if existing.term != nil {
				existing.term.Close()
				existing.term = nil
			}
			existing.attachStarted = false
			existing.terminalAddress = address
		}
		existing.convSession = sessionID
		return existing
	}
	p := &pane{terminalAddress: address, convSession: sessionID}
	a.panes[block.ID] = p
	return p
}

func (a *App) renderPanes(c *ui.Context) {
	tab, ok := a.activeTab()
	if !ok {
		if kit.Empty(c, "No tab", "Open a shell or conversation tab to begin.", "New Tab") {
			a.NewTerminal()
		}
		return
	}
	if len(tab.Root.Blocks()) == 0 {
		kit.Empty(c, "No pane in this tab", "The tab's layout is empty.", "")
		return
	}
	focused := a.focusedPane(tab)
	renderWorldNode(c, tab.Root, focused, func(c *ui.Context, block corelayout.Node) {
		a.renderBlock(c, block, focused)
	})
}

func (a *App) renderBlock(c *ui.Context, block corelayout.Node, focused string) {
	p := a.paneFor(block)
	theme := c.Theme()
	ui.Row(c).Fill().Padding(4).Children(func() {
		rail := ui.Box(c).Width(2).FillHeight()
		if rail.Clicked() {
			a.focusBlock(block.ID)
		}
		card := ui.Column(c).Grow(1).FillHeight().Background(kit.Card(theme)).Radius(10)
		if block.ID != "" && block.ID == focused {
			card.Border(1, theme.Accent)
		} else {
			card.Border(1, theme.Border)
		}
		card.Children(func() {
			a.paneHeader(c, block, p)
			a.renderBlockContent(c, block, p)
		})
	})
}

// paneHeader names a block and carries its view-only actions. The title area
// focuses the pane; the menu only rearranges or detaches views.
func (a *App) paneHeader(c *ui.Context, block corelayout.Node, p *pane) {
	theme := c.Theme()
	title, subtitle := a.blockTitle(block)
	ui.Row(c).FillWidth().Height(30).PaddingX(10).Gap(8).AlignItems(ui.Center).
		BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).Children(func() {
		area := ui.ButtonBase(c).Label(title).FillHeight()
		area.Children(func() {
			ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
				ui.Text(c, title).FontSize(12).FontWeight(600).NoWrap()
				if subtitle != "" {
					ui.Text(c, subtitle).FontSize(10).TextColor(theme.TextMuted).NoWrap().Ellipsis(subtitle)
				}
			})
		})
		if area.Clicked() {
			a.focusBlock(block.ID)
		}
		ui.Spacer(c)
		a.paneMenu(c, block, p)
	})
}

func (a *App) paneMenu(c *ui.Context, block corelayout.Node, p *pane) {
	ui.PopoverMenuButton(c, "···", func(m *ui.Menu) {
		if m.Item("Split right").Chosen() {
			a.focusBlock(block.ID)
			a.splitActive(corelayout.TypeRow)
		}
		if m.Item("Split down").Chosen() {
			a.focusBlock(block.ID)
			a.splitActive(corelayout.TypeColumn)
		}
		if m.Item("Close pane").Chosen() {
			a.closePane(block.ID)
		}
		if p.term != nil {
			if m.Item("Detach view").Chosen() {
				p.term.Close()
				p.term = nil
				p.attachStarted = false
			}
		}
	}).Label("Pane actions").FontSize(12).PaddingX(6).PaddingY(2)
}

func (a *App) blockTitle(block corelayout.Node) (string, string) {
	if address := blockTerminalAddress(block); address != "" {
		return "Terminal", address
	}
	if block.Content != nil && block.Content.Mod != nil {
		switch *block.Content.Mod {
		case "conversation":
			if sessionID := conversationBlockSession(block); sessionID != "" {
				return "Conversation", kit.ShortID(sessionID)
			}
			return "Conversation", "no session"
		case "files":
			return "Files", a.activeFolder()
		case "changes":
			return "Diff", a.activeFolder()
		default:
			return "Unsupported", *block.Content.Mod
		}
	}
	if block.Content != nil && block.Content.Tab != nil {
		return "Context", "content.tab"
	}
	return "Unsupported", "unknown content"
}

func (a *App) renderBlockContent(c *ui.Context, block corelayout.Node, p *pane) {
	if p.terminalAddress != "" {
		a.renderTerminal(c, p)
		return
	}
	if block.Content != nil && block.Content.Mod != nil {
		switch *block.Content.Mod {
		case "conversation":
			a.renderConversation(c, p)
		case "files":
			a.renderFiles(c, p)
		case "changes":
			a.renderDiff(c, p)
		default:
			a.renderUnsupported(c, block)
		}
		return
	}
	a.renderUnsupported(c, block)
}

// renderUnsupported names explicitly why a pane cannot be drawn. It never
// substitutes data the document does not carry.
func (a *App) renderUnsupported(c *ui.Context, block corelayout.Node) {
	detail := "This Desktop build cannot render this pane's content."
	if block.Content != nil {
		switch {
		case block.Content.Tab != nil:
			detail = "Context panes (content.tab) are not supported by this Desktop build."
		case block.Content.Mod != nil:
			detail = "The mod " + *block.Content.Mod + " is not supported by this Desktop build."
		case block.Content.Terminal == nil && block.Content.Agent == nil:
			detail = "The block has no recognized content variant."
		}
	}
	kit.Empty(c, "Unsupported pane", detail, "")
}

func (a *App) renderConversation(c *ui.Context, p *pane) {
	if p.convSession == "" {
		kit.Empty(c, "Conversation has no session",
			"This conversation block has no props.session, so no Agent can be addressed.", "")
		return
	}
	state := a.conversationFor(p.convSession)
	if state.Snapshot.Provider == "" && !state.Loading && state.ActionErr == "" {
		a.loadAgentSnapshot(state)
	}
	space, _ := a.activeSpace()
	sessionID := p.convSession
	conversation.View(c, state, conversation.Actions{
		Start:  func() { a.startAgent(sessionID, space.Folder) },
		Prompt: func(text string) { a.prompt(sessionID, text) },
		Abort:  func() { a.abortAgent(sessionID) },
		Stop:   func() { a.stopAgent(sessionID) },
		Approve: func(id string, decision agents.Decision) {
			a.approve(sessionID, id, string(decision))
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

func (a *App) renderTerminal(c *ui.Context, p *pane) {
	if p.terminalAddress == "" {
		kit.Empty(c, "Terminal has no address",
			"This block does not name a herdr-core terminal, so there is nothing to attach.", "")
		return
	}
	if p.term == nil {
		p.term = terminal.New(p.terminalAddress, p.terminalAddress, false)
	}
	if a.termLibErr != "" {
		kit.Banner(c, "danger", "Terminal engine unavailable: "+a.termLibErr, "")
	} else if !a.termLibReady {
		kit.Banner(c, "warn", "Loading the terminal engine…", "")
	}
	if a.hc == nil && !a.termLoading && !a.termTried && a.hcErr == "" {
		a.refreshTerminals()
	}
	if a.hc == nil && a.hcErr != "" {
		if kit.Banner(c, "danger", a.hcErr, "Retry") {
			a.hcErr = ""
			a.refreshTerminals()
		}
	}
	if a.hc != nil && a.termLibReady {
		a.attachPaneTerminal(p)
	}
	terminal.View(c, p.term)
}

func (a *App) activeFolder() string {
	space, ok := a.activeSpace()
	if !ok {
		return ""
	}
	return space.Folder
}

func (a *App) renderFiles(c *ui.Context, p *pane) {
	folder := a.activeFolder()
	if p.folder != folder {
		p.files = nil
		p.filesLoaded = false
		p.filesLoading = false
		p.folder = folder
	}
	target, message := a.repoFor(folder)
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
		a.openDiffTab()
	}, func(path string) {
		a.loadFileInto(p, path)
	})
}

func (a *App) loadFiles(p *pane) {
	p.filesLoading = true
	repo := p.files.Repo
	files := p.files
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		status, statusErr := repo.Status(ctx)
		nodes, treeErr := repo.Tree(ctx)
		a.update(func() {
			p.filesLoading = false
			if p.files != files {
				return
			}
			p.filesLoaded = true
			if statusErr != nil {
				files.Err = statusErr.Error()
			} else {
				files.Status = status
			}
			if treeErr != nil {
				files.Err = treeErr.Error()
			} else {
				files.Nodes = nodes
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
	files := p.files
	go func() {
		content, err := repo.Read(path)
		a.update(func() {
			if p.files == files {
				files.SetContent(path, content, err)
			}
		})
	}()
}

func (a *App) renderDiff(c *ui.Context, p *pane) {
	folder := a.activeFolder()
	if p.folder != folder {
		p.diff = nil
		p.diffLoaded = false
		p.diffLoading = false
		p.folder = folder
	}
	target, message := a.repoFor(folder)
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
	diff := p.diff
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		patch, err := target.Diff(ctx, repository.ScopeHead)
		a.update(func() {
			p.diffLoading = false
			if p.diff != diff {
				return
			}
			p.diffLoaded = true
			if err != nil {
				diff.Err = err.Error()
				return
			}
			diff.SetPatch(patch)
		})
	}()
}

// openDiffTab reveals a changes pane in the active Space. It changes document
// metadata only and starts no process.
func (a *App) openDiffTab() { a.newModTab("changes", "Diff") }

// blockTerminalAddress returns the terminal a block shows. A content.agent
// block names an agent's terminal and is attached the same way.
func blockTerminalAddress(block corelayout.Node) string {
	if block.Content == nil {
		return ""
	}
	if block.Content.Terminal != nil && *block.Content.Terminal != "" {
		return *block.Content.Terminal
	}
	if block.Content.Agent != nil && *block.Content.Agent != "" {
		return *block.Content.Agent
	}
	return ""
}

// conversationBlockSession reads props.session from a conversation mod block.
func conversationBlockSession(block corelayout.Node) string {
	if block.Content == nil || block.Content.Mod == nil || *block.Content.Mod != "conversation" {
		return ""
	}
	if len(block.Content.Props) == 0 {
		return ""
	}
	var props struct {
		Session string `json:"session"`
	}
	if err := json.Unmarshal(block.Content.Props, &props); err != nil {
		return ""
	}
	return props.Session
}

// newModBlock builds a block whose content is a mod with opaque props.
func newModBlock(mod string, props map[string]string) corelayout.Node {
	node := corelayout.Node{
		ID:      world.NewID("p"),
		Type:    corelayout.TypeBlock,
		Content: &corelayout.Content{Mod: &mod},
	}
	if len(props) > 0 {
		if raw, err := json.Marshal(props); err == nil {
			node.Content.Props = raw
		}
	}
	return node
}

// terminalBlock builds a terminal block for a core-owned terminal address.
func terminalBlock(address string) corelayout.Node {
	return corelayout.Node{
		ID:      world.NewID("p"),
		Type:    corelayout.TypeBlock,
		Content: &corelayout.Content{Terminal: &address},
	}
}

// terminalTab builds a tab whose root is one terminal block.
func terminalTab(name, address string) corelayout.SharedTab {
	return corelayout.SharedTab{
		ID:   world.NewID("tb"),
		Name: name,
		Root: terminalBlock(address),
	}
}
