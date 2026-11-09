// Package app owns the Desktop window's product state and composes the native
// views. Spaces, tabs and panes come from the machine-owned herdr-app document;
// the bridge owns Agent control and Remote Screen, and herdr-core owns the
// terminals. Closing a pane, tab or space edits that document's metadata only:
// it never stops a terminal or an Agent.
package app

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"time"

	"github.com/egoist/mygo"
	mygoterm "github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
	"github.com/suj1e/ukk/apps/desktop/internal/hc"
	"github.com/suj1e/ukk/apps/desktop/internal/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/conversation"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	"github.com/suj1e/ukk/apps/desktop/internal/world"
)

// App is the Desktop's state. Only the UI thread mutates it; background work
// publishes through Window.Update.
type App struct {
	dir string
	win *mygo.Window

	// world is the shared herdr-app document plus this installation's private
	// view state. It is nil until herdr-core has been reached and the document
	// loaded.
	world        coreWorld
	worldClient  *hc.Client
	worldErr     string
	worldLoading bool

	// focus is the view's focused block per tab. The document never stores it.
	focus map[string]string

	convs   map[string]*conversation.State
	panes   map[string]*pane
	repos   map[string]*repository.Repo
	repoErr map[string]string

	bridge     *bridge.Client
	bridgeErr  string
	bridgeCtx  context.Context
	cancelAll  context.CancelFunc
	liveAgents map[string]bridge.AgentSnapshot

	hc          *hc.Client
	hcHello     hc.HelloResult
	hcErr       string
	hcRetry     *time.Timer
	hcBackoff   time.Duration
	terminals   []hc.Terminal
	termAt      time.Time
	termLoading bool
	termTried   bool

	newSpace formState
	rename   renameState
	palette  paletteState
	settings settingsState
	notice   string

	theme        *ui.Theme
	themeDark    bool
	termLibReady bool
	termLibErr   string
}

type formState struct {
	Open   bool
	Name   string
	Folder string
	Busy   bool
	Err    string
}

// renameState is the modal that renames a space or tab through the world.
type renameState struct {
	Open    bool
	Kind    string
	SpaceID string
	ID      string
	Name    string
	Err     string
}

type paletteState struct {
	Open  bool
	Query string
}

type settingsState struct {
	Open       bool
	Tab        int
	Invite     *bridge.Invite
	QR         ui.ImageSource
	Status     bridge.Status
	Err        string
	Busy       bool
	Revoke     string
	Plugins    []extensions.PluginStatus
	PluginDir  string
	PluginErr  string
	PluginBusy bool
}

// New creates the application state. dir is the per-installation data
// directory; the shared world is loaded once herdr-core connects.
func New(dir string) *App {
	return &App{
		dir:        dir,
		focus:      map[string]string{},
		convs:      map[string]*conversation.State{},
		panes:      map[string]*pane{},
		repos:      map[string]*repository.Repo{},
		repoErr:    map[string]string{},
		liveAgents: map[string]bridge.AgentSnapshot{},
		theme:      kit.Theme(false),
	}
}

// Attach binds the window and starts the background work.
func (a *App) Attach(win *mygo.Window) {
	a.win = win
	a.bridgeCtx, a.cancelAll = context.WithCancel(context.Background())
	go a.connectBridge()
	go a.preloadTerminal()
	go a.pollHerdr()
	// Connect to herdr-core at startup: the core owns the terminals and the
	// shared world document, and the app is only useful once both are visible.
	a.refreshTerminals()
}

// Shutdown stops background work. It never stops an Agent or terminal.
func (a *App) Shutdown() {
	if a.cancelAll != nil {
		a.cancelAll()
	}
	a.stopHerdrRetry()
	for _, p := range a.panes {
		if p.term != nil {
			p.term.Close()
		}
	}
	if a.hc != nil {
		_ = a.hc.Close()
	}
}

// preloadTerminal fetches and loads libghostty-vt off the UI thread. Creating a
// terminal never blocks a frame on a download.
func (a *App) preloadTerminal() {
	if err := mygoterm.Load(); err != nil {
		a.update(func() { a.termLibErr = err.Error() })
		return
	}
	a.update(func() { a.termLibReady = true; a.termLibErr = "" })
}

func (a *App) update(fn func()) {
	if a.win == nil || a.win.IsDestroyed() {
		return
	}
	a.win.Update(fn)
}

// loadWorld reads the shared herdr-app document and this installation's view
// file. A failed read leaves the UI in an explicit retry state; it never
// fabricates an empty document.
func (a *App) loadWorld() {
	if a.worldLoading {
		return
	}
	client := a.hc
	if client == nil {
		return
	}
	a.worldLoading = true
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		loaded, err := world.Load(ctx, client, "", filepath.Join(a.dir, "view.json"))
		a.update(func() {
			a.worldLoading = false
			if loaded == nil {
				a.worldErr = err.Error()
				if a.world != nil {
					a.notice = "reload the shared world: " + firstLine(err.Error())
				}
				return
			}
			a.world = loaded
			a.worldClient = client
			a.worldErr = ""
			if err != nil {
				a.notice = firstLine(err.Error())
			}
			a.ensureFocus()
			a.prunePanes()
		})
	}()
}

// retryWorld restarts loading after an error shown in the UI.
func (a *App) retryWorld() {
	a.worldErr = ""
	if a.hc == nil {
		a.refreshTerminals()
		return
	}
	a.loadWorld()
}

func (a *App) spaces() []corelayout.SharedSpace {
	if a.world == nil {
		return nil
	}
	return a.world.Spaces()
}

func (a *App) activeSpace() (corelayout.SharedSpace, bool) {
	if a.world == nil {
		return corelayout.SharedSpace{}, false
	}
	return a.world.ActiveSpace()
}

func (a *App) activeTab() (corelayout.SharedTab, bool) {
	if a.world == nil {
		return corelayout.SharedTab{}, false
	}
	return a.world.ActiveTab()
}

// focusedPane is the view's focused block in tab, falling back to the first
// block. The value is local presentation state; it is saved with SaveView.
func (a *App) focusedPane(tab corelayout.SharedTab) string {
	id := a.focus[tab.ID]
	if id == "" && a.world != nil {
		id = a.world.FocusedPane(tab.ID)
	}
	if id == "" || tab.Root.Find(id) == nil {
		id = focusedPaneID(tab)
		if id != "" {
			a.focus[tab.ID] = id
		}
	}
	return id
}

func (a *App) ensureFocus() {
	tab, ok := a.activeTab()
	if !ok {
		a.focus = map[string]string{}
		return
	}
	if id := a.focusedPane(tab); id != "" {
		a.focus[tab.ID] = id
	}
}

func (a *App) selectSpace(id string) {
	if a.world == nil || !a.world.SelectSpace(id) {
		return
	}
	a.ensureFocus()
	a.persistView()
}

func (a *App) selectTab(id string) {
	space, ok := a.activeSpace()
	if !ok {
		return
	}
	if !a.world.SelectTab(space.ID, id) {
		return
	}
	a.ensureFocus()
	a.persistView()
}

func (a *App) focusBlock(id string) {
	tab, ok := a.activeTab()
	if !ok || id == "" {
		return
	}
	if !a.world.FocusPane(tab.ID, id) {
		return
	}
	a.focus[tab.ID] = id
	a.persistView()
}

// persistView writes the private active space/tab and focus state.
func (a *App) persistView() {
	if a.world == nil {
		return
	}
	if err := a.world.SaveView(); err != nil {
		a.notice = "save the view: " + err.Error()
	}
}

// saveWorld writes the shared document with its version CAS. A conflict means
// another client changed the world; the local copy is reloaded, never
// force-written.
func (a *App) saveWorld() {
	w := a.world
	if w == nil {
		return
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		if err := w.Save(ctx); err != nil {
			a.update(func() {
				if errors.Is(err, world.ErrConflict) {
					a.notice = "The shared world changed in another client; reloading."
					a.loadWorld()
					return
				}
				a.notice = "save the world: " + err.Error()
			})
		}
	}()
}

// prunePanes drops presentation state, and terminal attachments, for blocks
// that no longer exist. Dropping an attachment never stops the terminal.
func (a *App) prunePanes() {
	if a.world == nil {
		return
	}
	alive := map[string]bool{}
	doc := a.world.Document()
	for _, space := range doc.Spaces {
		for _, tab := range space.Tabs {
			for _, block := range tab.Root.Blocks() {
				alive[block.ID] = true
			}
		}
	}
	for id, p := range a.panes {
		if alive[id] {
			continue
		}
		if p.term != nil {
			p.term.Close()
		}
		delete(a.panes, id)
	}
}

func (a *App) conversationFor(sessionID string) *conversation.State {
	state, ok := a.convs[sessionID]
	if !ok {
		state = conversation.New(sessionID)
		a.convs[sessionID] = state
	}
	return state
}

// connectBridge starts the local bridge, links events, and retries while the
// application is alive.
func (a *App) connectBridge() {
	backoff := time.Second
	for {
		select {
		case <-a.bridgeCtx.Done():
			return
		default:
		}
		client, err := bridge.Ensure(a.bridgeCtx, a.dir)
		if err != nil {
			a.update(func() { a.bridgeErr = err.Error() })
			select {
			case <-a.bridgeCtx.Done():
				return
			case <-time.After(backoff):
			}
			if backoff < 15*time.Second {
				backoff *= 2
			}
			continue
		}
		backoff = time.Second
		a.update(func() {
			a.bridge = client
			a.bridgeErr = ""
			a.loadAgents()
		})
		events, err := client.Events(a.bridgeCtx, "")
		if err != nil {
			a.update(func() { a.bridgeErr = err.Error() })
			select {
			case <-a.bridgeCtx.Done():
				return
			case <-time.After(2 * time.Second):
			}
			continue
		}
		for event := range events {
			event := event
			a.update(func() { a.applyAgentEvent(event) })
		}
		a.update(func() { a.bridgeErr = "event stream disconnected; reconnecting" })
		select {
		case <-a.bridgeCtx.Done():
			return
		case <-time.After(2 * time.Second):
		}
	}
}

func (a *App) applyAgentEvent(event bridge.Event) {
	if event.Type == "pairing" || event.Type == "pairing_complete" || event.Type == "screen" {
		a.loadRemoteStatus()
	}
	if event.Agent == nil || event.SessionID == "" {
		if event.Type == bridge.EventError {
			a.notice = event.Error
		}
		return
	}
	state := a.conversationFor(event.SessionID)
	state.Snapshot = toAgentSnapshot(*event.Agent)
	state.Loading = false
	state.ActionErr = ""
	a.liveAgents[event.SessionID] = *event.Agent
	if event.Type == bridge.EventError {
		state.ActionErr = event.Error
	}
}

// loadAgents reads every provider session the bridge owns so the sidebar
// reflects Agents started from another window or before a restart.
func (a *App) loadAgents() {
	client := a.bridge
	if client == nil {
		return
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 10*time.Second)
		defer cancel()
		list, err := client.AgentList(ctx)
		if err != nil {
			return
		}
		next := make(map[string]bridge.AgentSnapshot, len(list))
		for _, snapshot := range list {
			if snapshot.SessionID != "" {
				next[snapshot.SessionID] = snapshot
			}
		}
		a.update(func() { a.liveAgents = next })
	}()
}

// startAgent starts the real provider session for a conversation pane.
func (a *App) startAgent(sessionID, cwd string) {
	if a.bridge == nil {
		a.notice = "The bridge is unavailable; the Agent cannot start."
		return
	}
	if sessionID == "" {
		return
	}
	state := a.conversationFor(sessionID)
	if state.Loading {
		return
	}
	state.Loading = true
	state.ActionErr = ""
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 60*time.Second)
		defer cancel()
		snapshot, err := client.StartAgent(ctx, bridge.StartParams{SessionID: sessionID, Cwd: cwd, Adapter: "omp"})
		a.update(func() {
			state.Loading = false
			if err != nil {
				state.ActionErr = err.Error()
				return
			}
			state.Snapshot = toAgentSnapshot(snapshot)
			a.liveAgents[sessionID] = snapshot
			a.loadAgents()
		})
	}()
}

func (a *App) prompt(sessionID, text string) {
	if a.bridge == nil || sessionID == "" {
		return
	}
	client := a.bridge
	state := a.conversationFor(sessionID)
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 30*time.Second)
		defer cancel()
		if err := client.Prompt(ctx, sessionID, text); err != nil {
			a.update(func() { state.ActionErr = err.Error() })
		}
	}()
}

func (a *App) abortAgent(sessionID string) {
	if a.bridge == nil || sessionID == "" {
		return
	}
	client := a.bridge
	state := a.conversationFor(sessionID)
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		if err := client.Abort(ctx, sessionID); err != nil {
			a.update(func() { state.ActionErr = err.Error() })
		}
	}()
}

func (a *App) stopAgent(sessionID string) {
	if a.bridge == nil || sessionID == "" {
		return
	}
	client := a.bridge
	state := a.conversationFor(sessionID)
	state.Stopping = false
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		if err := client.Stop(ctx, sessionID); err != nil {
			a.update(func() { state.ActionErr = err.Error() })
		}
	}()
}

func (a *App) approve(sessionID, id, decision string) {
	if a.bridge == nil || sessionID == "" {
		return
	}
	client := a.bridge
	state := a.conversationFor(sessionID)
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		err := client.Approve(ctx, bridge.ApproveParams{SessionID: sessionID, ApprovalID: id, Decision: decision})
		if err != nil {
			a.update(func() { state.ActionErr = err.Error() })
		}
	}()
}

// refreshTerminals connects to herdr-core when needed and lists terminals.
// A failed connect or a lost connection is retried with capped backoff.
func (a *App) refreshTerminals() {
	if a.termLoading {
		return
	}
	if a.hcErr == "" && a.hc != nil && time.Since(a.termAt) < 2*time.Second {
		return
	}
	client := a.hc
	if client != nil {
		select {
		case <-client.Done():
			client = nil
			a.hc = nil
		default:
		}
	}
	a.termLoading = true
	a.termTried = true
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		if client == nil {
			connected, err := hc.Connect(a.bridgeCtx, hc.Options{Timeout: 10 * time.Second})
			if err != nil {
				a.update(func() {
					a.termLoading = false
					a.hcErr = err.Error()
					a.scheduleHerdrRetry()
				})
				return
			}
			client = connected
			hello := connected.Hello()
			a.update(func() {
				a.hc = connected
				a.hcHello = hello
				a.hcErr = ""
				a.hcBackoff = 0
				a.stopHerdrRetry()
				if a.world == nil || a.worldClient != connected {
					a.loadWorld()
				}
			})
			go a.watchHerdr(connected)
		}
		result, err := client.TerminalList(ctx, hc.TerminalListOptions{All: false})
		if err != nil {
			a.update(func() {
				a.termLoading = false
				a.hcErr = err.Error()
				a.scheduleHerdrRetry()
			})
			return
		}
		a.update(func() {
			a.termLoading = false
			a.terminals = result.Terminals
			a.termAt = time.Now()
			a.hcErr = ""
			// The document is read over the same connection; reload it when
			// this is a new client so other clients' edits become visible.
			if a.world == nil || a.worldClient != client {
				a.loadWorld()
			}
		})
	}()
}

// watchHerdr notices a lost core connection and reconnects. Terminals live in
// herdr-core, so losing this client never stops one.
func (a *App) watchHerdr(client *hc.Client) {
	select {
	case <-a.bridgeCtx.Done():
		return
	case <-client.Done():
	}
	a.update(func() {
		if a.hc != client {
			return
		}
		a.hc = nil
		a.worldClient = nil
		a.hcHello = hc.HelloResult{}
		a.terminals = nil
		for _, p := range a.panes {
			if p.term != nil {
				p.term.Close()
				p.term = nil
				p.attachStarted = false
			}
		}
		a.hcErr = "lost the herdr-core connection"
		a.scheduleHerdrRetry()
	})
}

// scheduleHerdrRetry retries a failed connect or a lost connection with capped
// backoff. The caller runs on the UI thread.
func (a *App) scheduleHerdrRetry() {
	if a.hcRetry != nil || a.bridgeCtx.Err() != nil {
		return
	}
	delay := a.hcBackoff
	if delay <= 0 {
		delay = time.Second
	}
	a.hcBackoff = min(delay*2, 30*time.Second)
	a.hcRetry = time.AfterFunc(delay, func() {
		a.update(func() {
			a.hcRetry = nil
			a.refreshTerminals()
		})
	})
}

func (a *App) stopHerdrRetry() {
	if a.hcRetry != nil {
		a.hcRetry.Stop()
		a.hcRetry = nil
	}
}

// runShell starts a shell in cwd through herdr-core and hands its terminal
// address to done on the UI thread. It never waits for the shell to exit.
func (a *App) runShell(cwd string, done func(address string), fail func(error)) {
	client := a.hc
	if client == nil {
		if fail != nil {
			fail(errors.New("herdr-core is not connected yet"))
		}
		a.refreshTerminals()
		return
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		result, err := client.TerminalRun(ctx, hc.RunOptions{
			Argv:   defaultShell(),
			Cwd:    cwd,
			Caller: "ukk-desktop",
			Keep:   true,
			Size:   &hc.InitialSize{Cols: 100, Rows: 30},
		})
		a.update(func() {
			if err != nil {
				if fail != nil {
					fail(err)
				}
				return
			}
			a.notice = ""
			if done != nil {
				done(result.Address)
			}
			a.refreshTerminals()
		})
	}()
}

// shellCwd is the focused pane's terminal folder when it is known, otherwise
// the active Space's folder.
func (a *App) shellCwd(space corelayout.SharedSpace) string {
	tab, ok := a.activeTab()
	if ok {
		if id := a.focusedPane(tab); id != "" {
			if block := tab.Root.Find(id); block != nil {
				if address := blockTerminalAddress(*block); address != "" {
					for _, term := range a.terminals {
						if term.Address == address && term.Cwd != "" {
							return term.Cwd
						}
					}
				}
			}
		}
	}
	return space.Folder
}

// NewTerminal opens a shell tab in the active Space. Closing it detaches; it
// never stops the shell.
func (a *App) NewTerminal() {
	if a.world == nil {
		a.notice = "The shared world is not loaded yet."
		return
	}
	space, ok := a.activeSpace()
	if !ok {
		a.notice = "Create a Space before opening a terminal."
		return
	}
	cwd := a.shellCwd(space)
	a.runShell(cwd, func(address string) {
		tab := terminalTab("Terminal", address)
		if err := a.world.AddTab(space.ID, tab); err != nil {
			a.notice = err.Error()
			return
		}
		a.ensureFocus()
		a.saveWorld()
		a.persistView()
	}, func(err error) { a.notice = "terminal: " + err.Error() })
}

// newConversationTab opens a tab whose root is a conversation mod block.
func (a *App) newConversationTab() {
	if a.world == nil {
		a.notice = "The shared world is not loaded yet."
		return
	}
	space, ok := a.activeSpace()
	if !ok {
		a.notice = "Create a Space before opening a tab."
		return
	}
	sessionID := world.NewID("p")
	tab := corelayout.SharedTab{
		ID:   world.NewID("tb"),
		Name: "Chat",
		Root: newModBlock("conversation", map[string]string{"session": sessionID}),
	}
	if err := a.world.AddTab(space.ID, tab); err != nil {
		a.notice = err.Error()
		return
	}
	a.ensureFocus()
	a.saveWorld()
	a.persistView()
}

// newModTab opens a tab for a repository mod such as files or changes.
func (a *App) newModTab(mod, name string) {
	if a.world == nil {
		a.notice = "The shared world is not loaded yet."
		return
	}
	space, ok := a.activeSpace()
	if !ok {
		a.notice = "Create a Space before opening a tab."
		return
	}
	tab := corelayout.SharedTab{
		ID:   world.NewID("tb"),
		Name: name,
		Root: newModBlock(mod, nil),
	}
	if err := a.world.AddTab(space.ID, tab); err != nil {
		a.notice = err.Error()
		return
	}
	a.ensureFocus()
	a.saveWorld()
	a.persistView()
}

// splitActive replaces the focused pane with a row or column holding it and a
// new shell. axis is "row" or "column".
func (a *App) splitActive(axis string) {
	if a.world == nil {
		a.notice = "The shared world is not loaded yet."
		return
	}
	space, ok := a.activeSpace()
	if !ok {
		a.notice = "Create a Space before splitting a pane."
		return
	}
	tab, ok := a.activeTab()
	if !ok {
		a.notice = "Open a tab before splitting a pane."
		return
	}
	target := a.focusedPane(tab)
	if target == "" {
		a.notice = "Focus a pane before splitting."
		return
	}
	cwd := a.shellCwd(space)
	a.runShell(cwd, func(address string) {
		block := terminalBlock(address)
		if err := a.world.Split(space.ID, tab.ID, target, axis, block); err != nil {
			a.notice = err.Error()
			return
		}
		if a.world.FocusPane(tab.ID, block.ID) {
			a.focus[tab.ID] = block.ID
		}
		a.saveWorld()
		a.persistView()
	}, func(err error) { a.notice = "terminal: " + err.Error() })
}

// NewTab opens a terminal tab, the default surface of a Space.
func (a *App) NewTab() { a.NewTerminal() }

// SplitRight opens a terminal pane to the right of the focused pane.
func (a *App) SplitRight() { a.splitActive(corelayout.TypeRow) }

// SplitDown opens a terminal pane below the focused pane.
func (a *App) SplitDown() { a.splitActive(corelayout.TypeColumn) }

// CloseActivePane removes the focused pane's metadata. It never signals the
// terminal the pane showed.
func (a *App) CloseActivePane() {
	tab, ok := a.activeTab()
	if !ok {
		return
	}
	a.closePane(a.focusedPane(tab))
}

func (a *App) closePane(id string) {
	space, ok := a.activeSpace()
	if !ok || id == "" {
		return
	}
	tab, ok := a.activeTab()
	if !ok {
		return
	}
	// The last pane closes its tab, and the last tab closes its space, as in
	// Herdr; only layout metadata is removed.
	if len(tab.Root.Blocks()) <= 1 {
		a.closeTab(tab.ID)
		return
	}
	if err := a.world.RemovePane(space.ID, tab.ID, id); err != nil {
		a.notice = err.Error()
		return
	}
	delete(a.focus, tab.ID)
	a.prunePanes()
	a.ensureFocus()
	a.saveWorld()
	a.persistView()
}

func (a *App) closeTab(id string) {
	space, ok := a.activeSpace()
	if !ok {
		return
	}
	if len(space.Tabs) == 1 {
		a.closeSpace(space.ID)
		return
	}
	if err := a.world.RemoveTab(space.ID, id); err != nil {
		a.notice = err.Error()
		return
	}
	delete(a.focus, id)
	a.prunePanes()
	a.ensureFocus()
	a.saveWorld()
	a.persistView()
}

func (a *App) closeSpace(id string) {
	if a.world == nil {
		return
	}
	if err := a.world.RemoveSpace(id); err != nil {
		a.notice = err.Error()
		return
	}
	a.prunePanes()
	a.ensureFocus()
	a.saveWorld()
	a.persistView()
}

func (a *App) openSpaceRename(space corelayout.SharedSpace) {
	a.rename = renameState{Open: true, Kind: "space", SpaceID: space.ID, ID: space.ID, Name: spaceDisplayName(space)}
}

func (a *App) openTabRename(space corelayout.SharedSpace, tab corelayout.SharedTab) {
	a.rename = renameState{Open: true, Kind: "tab", SpaceID: space.ID, ID: tab.ID, Name: tab.Name}
}

func (a *App) renameSubmit() {
	form := &a.rename
	if a.world == nil {
		form.Err = "The shared world is not loaded yet."
		return
	}
	name := strings.TrimSpace(form.Name)
	if name == "" {
		form.Err = "A name is required."
		return
	}
	var err error
	switch form.Kind {
	case "space":
		err = a.world.RenameSpace(form.SpaceID, name)
	case "tab":
		err = a.world.RenameTab(form.SpaceID, form.ID, name)
	default:
		err = errors.New("nothing to rename")
	}
	if err != nil {
		form.Err = err.Error()
		return
	}
	form.Open = false
	form.Err = ""
	a.saveWorld()
}

// openTerminal shows a terminal the core already runs, reusing a pane that
// already shows it or opening a tab in the active Space.
func (a *App) openTerminal(address string) {
	if address == "" || a.world == nil {
		return
	}
	if spaceID, tabID, paneID, ok := a.findBlock(func(block corelayout.Node) bool {
		return blockTerminalAddress(block) == address
	}); ok {
		a.revealBlock(spaceID, tabID, paneID)
		return
	}
	space, ok := a.activeSpace()
	if !ok {
		a.notice = "Create a Space before opening a terminal."
		return
	}
	tab := terminalTab("Terminal", address)
	if err := a.world.AddTab(space.ID, tab); err != nil {
		a.notice = err.Error()
		return
	}
	a.ensureFocus()
	if id := focusedPaneID(tab); id != "" {
		a.focus[tab.ID] = id
		a.world.FocusPane(tab.ID, id)
	}
	a.saveWorld()
	a.persistView()
}

// openSessionAgent reveals the conversation pane for a bridge session,
// creating a conversation tab when none shows it yet.
func (a *App) openSessionAgent(sessionID string) {
	if sessionID == "" || a.world == nil {
		return
	}
	if spaceID, tabID, paneID, ok := a.findBlock(func(block corelayout.Node) bool {
		return conversationBlockSession(block) == sessionID
	}); ok {
		a.revealBlock(spaceID, tabID, paneID)
		return
	}
	space, ok := a.activeSpace()
	if !ok {
		a.notice = "Create a Space before opening a conversation."
		return
	}
	tab := corelayout.SharedTab{
		ID:   world.NewID("tb"),
		Name: "Chat",
		Root: newModBlock("conversation", map[string]string{"session": sessionID}),
	}
	if err := a.world.AddTab(space.ID, tab); err != nil {
		a.notice = err.Error()
		return
	}
	a.ensureFocus()
	a.saveWorld()
	a.persistView()
}

// findBlock finds the first block matching match and returns its location.
func (a *App) findBlock(match func(corelayout.Node) bool) (spaceID, tabID, paneID string, ok bool) {
	if a.world == nil {
		return "", "", "", false
	}
	doc := a.world.Document()
	for _, space := range doc.Spaces {
		for _, tab := range space.Tabs {
			for _, block := range tab.Root.Blocks() {
				if match(*block) {
					return space.ID, tab.ID, block.ID, true
				}
			}
		}
	}
	return "", "", "", false
}

// revealBlock selects the space and tab and focuses the pane.
func (a *App) revealBlock(spaceID, tabID, paneID string) {
	if a.world == nil {
		return
	}
	if !a.world.SelectTab(spaceID, tabID) {
		return
	}
	a.ensureFocus()
	if a.world.FocusPane(tabID, paneID) {
		a.focus[tabID] = paneID
	}
	a.persistView()
}

func (a *App) tabTitleForSession(sessionID string) string {
	if a.world == nil || sessionID == "" {
		return ""
	}
	doc := a.world.Document()
	for _, space := range doc.Spaces {
		for _, tab := range space.Tabs {
			for _, block := range tab.Root.Blocks() {
				if conversationBlockSession(*block) == sessionID {
					return tab.Name
				}
			}
		}
	}
	return ""
}

func (a *App) pollHerdr() {
	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-a.bridgeCtx.Done():
			return
		case <-ticker.C:
			a.update(func() { a.refreshTerminals() })
		}
	}
}

func defaultShell() []string {
	if runtime.GOOS == "windows" {
		if comspec := os.Getenv("COMSPEC"); comspec != "" {
			return []string{comspec}
		}
		return []string{"cmd.exe"}
	}
	if shell := os.Getenv("SHELL"); shell != "" {
		return []string{shell}
	}
	return []string{"/bin/sh"}
}

func (a *App) newSpaceSubmit() {
	form := &a.newSpace
	if form.Busy {
		return
	}
	if a.world == nil {
		form.Err = "The shared world is not loaded yet."
		return
	}
	name := strings.TrimSpace(form.Name)
	folder := strings.TrimSpace(form.Folder)
	if name == "" || folder == "" {
		form.Err = "A name and a project folder are required."
		return
	}
	form.Busy = true
	form.Err = ""
	a.runShell(folder, func(address string) {
		space := corelayout.SharedSpace{
			ID:     world.NewID("sp"),
			Name:   name,
			Folder: folder,
			Tabs: []corelayout.SharedTab{{
				ID:   world.NewID("tb"),
				Name: "Terminal",
				Root: terminalBlock(address),
			}},
		}
		if err := a.world.AddSpace(space); err != nil {
			form.Busy = false
			form.Err = err.Error()
			return
		}
		form.Busy = false
		form.Open = false
		form.Name, form.Folder = "", ""
		a.ensureFocus()
		a.saveWorld()
		a.persistView()
	}, func(err error) {
		form.Busy = false
		form.Err = err.Error()
	})
}

// NewSpace opens the new-Space form.
func (a *App) NewSpace() {
	a.newSpace.Open = true
	a.newSpace.Err = ""
}

func (a *App) loadPlugins() {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		plugins, err := client.Plugins(ctx)
		a.update(func() {
			if err != nil {
				a.settings.PluginErr = err.Error()
				return
			}
			a.settings.Plugins = plugins
			a.settings.PluginErr = ""
		})
	}()
}

func (a *App) installPlugin() {
	if a.bridge == nil || a.settings.PluginBusy {
		return
	}
	dir := a.settings.PluginDir
	a.settings.PluginBusy = true
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		_, err := client.InstallPlugin(ctx, dir)
		a.update(func() {
			a.settings.PluginBusy = false
			if err != nil {
				a.settings.PluginErr = err.Error()
				return
			}
			a.settings.PluginDir = ""
			a.settings.PluginErr = ""
			a.loadPlugins()
		})
	}()
}

func (a *App) setPluginEnabled(id string, enabled bool) {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		var err error
		if enabled {
			err = client.EnablePlugin(ctx, id)
		} else {
			err = client.DisablePlugin(ctx, id)
		}
		a.update(func() {
			if err != nil {
				a.settings.PluginErr = err.Error()
			} else {
				a.settings.PluginErr = ""
			}
			a.loadPlugins()
		})
	}()
}

func (a *App) loadRemoteStatus() {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		status, err := client.RemoteStatus(ctx)
		a.update(func() {
			if err != nil {
				a.settings.Err = err.Error()
				return
			}
			a.settings.Status = status
			a.settings.Err = ""
		})
	}()
}

func (a *App) createInvite() {
	if a.bridge == nil {
		a.settings.Err = "The bridge is unavailable."
		return
	}
	client := a.bridge
	a.settings.Busy = true
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		invite, err := client.Invite(ctx)
		a.update(func() {
			a.settings.Busy = false
			if err != nil {
				a.settings.Err = err.Error()
				return
			}
			a.settings.Invite = &invite
			a.settings.Err = ""
			if len(invite.QRPNG) > 0 {
				source, decodeErr := ui.DecodeImage(invite.QRPNG)
				if decodeErr == nil {
					a.settings.QR = source
				}
			}
		})
	}()
}

func (a *App) grantDevice(id, scope string, minutes int) {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		err := client.Grant(ctx, bridge.GrantParams{DeviceID: id, Scope: scope, Minutes: minutes})
		a.update(func() {
			if err != nil {
				a.settings.Err = err.Error()
				return
			}
			a.settings.Err = ""
			a.loadRemoteStatus()
		})
	}()
}

func (a *App) confirmPairing(approve bool) {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 10*time.Second)
		defer cancel()
		err := client.Confirm(ctx, approve)
		a.update(func() {
			if err != nil {
				a.settings.Err = err.Error()
			}
			a.loadRemoteStatus()
		})
	}()
}

func (a *App) revokeDevice(id string) {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 15*time.Second)
		defer cancel()
		err := client.Revoke(ctx, id)
		a.update(func() {
			a.settings.Revoke = ""
			if err != nil {
				a.settings.Err = err.Error()
				return
			}
			a.loadRemoteStatus()
		})
	}()
}

// repoFor returns a cached repository handle for a Space's folder.
func (a *App) repoFor(folder string) (*repository.Repo, string) {
	if folder == "" {
		return nil, "This Space has no project folder."
	}
	if repo, ok := a.repos[folder]; ok {
		return repo, ""
	}
	if message, ok := a.repoErr[folder]; ok {
		return nil, message
	}
	repo, err := repository.Open(folder, repository.Compact())
	if err != nil {
		a.repoErr[folder] = err.Error()
		return nil, err.Error()
	}
	a.repos[folder] = repo
	return repo, ""
}

func (a *App) sortedTerminals() []hc.Terminal {
	out := append([]hc.Terminal(nil), a.terminals...)
	sort.Slice(out, func(i, j int) bool {
		if out[i].State != out[j].State {
			return out[i].State < out[j].State
		}
		return out[i].Address < out[j].Address
	})
	return out
}

func (a *App) terminalLabel(t hc.Terminal) string {
	switch {
	case t.Name != nil && *t.Name != "":
		return *t.Name
	case t.Title != "":
		return t.Title
	case t.Foreground != nil && t.Foreground.Name != "":
		return t.Foreground.Name
	default:
		return t.Address
	}
}

func (a *App) attachPaneTerminal(p *pane) {
	if p.term == nil || p.attachStarted || a.hc == nil {
		return
	}
	p.attachStarted = true
	address := p.term.Address
	client := a.hc
	state := p.term
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		attachment, err := client.Attach(ctx, address, 120, 36, hc.AttachOptions{})
		if err != nil {
			a.update(func() {
				if p.term != state {
					return
				}
				state.Err = err.Error()
				state.Connecting = false
				p.attachStarted = false
			})
			return
		}
		a.update(func() {
			if p.term != state {
				attachment.Close()
				return
			}
			if err := state.Attach(attachment); err != nil {
				a.notice = err.Error()
			}
		})
	}()
}

// OpenSettings opens the settings sheet.
func (a *App) OpenSettings() {
	a.settings.Open = true
	a.loadRemoteStatus()
	a.loadPlugins()
}
