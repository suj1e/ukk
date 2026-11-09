// Package app owns the Desktop window's product state and composes the native
// views. It talks to the local bridge for Agent control and Remote Screen, and
// to a local herdr-core client for terminal attachment.
package app

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"runtime"
	"sort"
	"time"

	"github.com/egoist/mygo"
	mygoterm "github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
	"github.com/suj1e/ukk/apps/desktop/internal/hc"
	"github.com/suj1e/ukk/apps/desktop/internal/repository"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/conversation"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/terminal"
	uispace "github.com/suj1e/ukk/apps/desktop/internal/ui/workspace"
	domain "github.com/suj1e/ukk/apps/desktop/internal/workspace"
)

const bridgeTimeout = 20 * time.Second

// App is the Desktop's state. Only the UI thread mutates it; background work
// publishes through Window.Update.
type App struct {
	store *domain.Store
	dir   string
	win   *mygo.Window

	meta   domain.Snapshot
	layout domain.SessionLayout

	spaceID    string
	sessionID  string
	activeTab  string
	activePane string

	convs   map[string]*conversation.State
	panes   map[string]*pane
	sizes   uispace.PaneSizes
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

	newSpace   formState
	newSession formState
	palette    paletteState
	settings   settingsState
	notice     string

	theme        *ui.Theme
	themeDark    bool
	termLibReady bool
	termLibErr   string
}

type formState struct {
	Open  bool
	Title string
	Root  string
	Busy  bool
	Err   string
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
	ScreenMsg  string
	Plugins    []extensions.PluginStatus
	PluginDir  string
	PluginErr  string
	PluginBusy bool
}

// New creates the application state.
func New(store *domain.Store, dir string) *App {
	return &App{
		store:      store,
		dir:        dir,
		convs:      map[string]*conversation.State{},
		panes:      map[string]*pane{},
		sizes:      uispace.PaneSizes{},
		repos:      map[string]*repository.Repo{},
		repoErr:    map[string]string{},
		liveAgents: map[string]bridge.AgentSnapshot{},
		theme:      kit.Theme(false),
	}
}

// Attach binds the window and loads initial state.
func (a *App) Attach(win *mygo.Window) {
	a.win = win
	a.meta = a.store.Snapshot()
	a.pickSelection()
	a.ensureLayout()
	a.bridgeCtx, a.cancelAll = context.WithCancel(context.Background())
	go a.watchStore()
	go a.connectBridge()
	go a.preloadTerminal()
	// Like the official client, connect to herdr-core at startup instead of
	// waiting for a terminal pane: the core owns the terminals, and the app is
	// only useful once their list is visible.
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

func (a *App) watchStore() {
	changes := a.store.Subscribe()
	for {
		select {
		case <-a.bridgeCtx.Done():
			return
		case _, ok := <-changes:
			if !ok {
				return
			}
			a.update(func() { a.reload() })
		}
	}
}

// reload re-reads durable state after a mutation from this or another window.
func (a *App) reload() {
	a.meta = a.store.Snapshot()
	a.pickSelection()
	a.ensureLayout()
	a.pushWorkspace()
}

func (a *App) pickSelection() {
	if a.spaceID == "" || !a.hasSpace(a.spaceID) {
		a.spaceID = ""
		if len(a.meta.Spaces) > 0 {
			a.spaceID = a.meta.Spaces[0].ID
		}
	}
	if a.sessionID == "" || !a.hasSession(a.sessionID) {
		a.sessionID = ""
		for _, session := range a.meta.Sessions {
			if session.SpaceID == a.spaceID {
				a.sessionID = session.ID
				break
			}
		}
	}
}

func (a *App) hasSpace(id string) bool {
	for _, space := range a.meta.Spaces {
		if space.ID == id {
			return true
		}
	}
	return false
}

func (a *App) hasSession(id string) bool {
	for _, session := range a.meta.Sessions {
		if session.ID == id {
			return true
		}
	}
	return false
}

func (a *App) session() (domain.Session, bool) {
	for _, session := range a.meta.Sessions {
		if session.ID == a.sessionID {
			return session, true
		}
	}
	return domain.Session{}, false
}

func (a *App) space() (domain.Space, bool) {
	for _, space := range a.meta.Spaces {
		if space.ID == a.spaceID {
			return space, true
		}
	}
	return domain.Space{}, false
}

func (a *App) agents() []domain.AgentRun {
	var out []domain.AgentRun
	for _, agent := range a.meta.Agents {
		if agent.SessionID == a.sessionID {
			out = append(out, agent)
		}
	}
	return out
}

func (a *App) ensureLayout() {
	if a.sessionID == "" {
		a.layout = domain.SessionLayout{}
		a.activeTab = ""
		a.activePane = ""
		return
	}
	layout, ok := a.meta.Layouts[a.sessionID]
	if !ok {
		return
	}
	if layout.Revision == a.layout.Revision && a.layout.SessionID == a.sessionID {
		return
	}
	a.layout = layout
	if a.activeTab == "" || !a.hasTab(a.activeTab) {
		a.activeTab = layout.ActiveTab
		if a.activeTab == "" && len(layout.Tabs) > 0 {
			a.activeTab = layout.Tabs[0].ID
		}
	}
	if a.activePane == "" || a.tabRoot().Find(a.activePane) == nil {
		if leaves := a.tabRoot().Leaves(); len(leaves) > 0 {
			a.activePane = leaves[0].ID
		} else {
			a.activePane = ""
		}
	}
	a.prunePanes()
}

func (a *App) hasTab(id string) bool {
	for _, tab := range a.layout.Tabs {
		if tab.ID == id {
			return true
		}
	}
	return false
}

func (a *App) tabRoot() *domain.PaneNode {
	for _, tab := range a.layout.Tabs {
		if tab.ID == a.activeTab {
			return tab.Root
		}
	}
	return nil
}

func (a *App) mutateLayout(fn func(*domain.SessionLayout) error) {
	if a.sessionID == "" {
		return
	}
	layout, ok := a.store.Layout(a.sessionID)
	if !ok {
		return
	}
	if err := fn(&layout); err != nil {
		a.notice = err.Error()
		return
	}
	saved, err := a.store.SaveLayout(a.sessionID, layout, layout.Revision)
	if err != nil {
		if errors.Is(err, domain.ErrConflict) {
			a.meta = a.store.Snapshot()
			a.ensureLayout()
			a.notice = "The layout changed in another window; reloaded."
			return
		}
		a.notice = err.Error()
		return
	}
	a.layout = saved
	a.prunePanes()
}

func (a *App) prunePanes() {
	alive := map[string]bool{}
	for _, tab := range a.layout.Tabs {
		for _, leaf := range tab.Root.Leaves() {
			alive[leaf.ID] = true
		}
	}
	for id, pane := range a.panes {
		if alive[id] {
			continue
		}
		if pane.term != nil {
			pane.term.Close()
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

func (a *App) context() (context.Context, context.CancelFunc) {
	return context.WithTimeout(a.bridgeCtx, bridgeTimeout)
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
			a.pushWorkspace()
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

func (a *App) pushWorkspace() {
	if a.bridge == nil {
		return
	}
	snapshot := a.store.Snapshot()
	raw, err := json.Marshal(snapshot)
	if err != nil {
		return
	}
	client := a.bridge
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 10*time.Second)
		defer cancel()
		if err := client.SetWorkspace(ctx, raw); err != nil {
			a.update(func() { a.bridgeErr = "workspace sync: " + err.Error() })
		}
	}()
}

// startAgent starts the real provider session for the selected Session.
func (a *App) startAgent() {
	if a.bridge == nil {
		a.notice = "The bridge is unavailable; the Agent cannot start."
		return
	}
	space, ok := a.space()
	if !ok || a.sessionID == "" {
		return
	}
	state := a.conversationFor(a.sessionID)
	if state.Loading {
		return
	}
	state.Loading = true
	state.ActionErr = ""
	client := a.bridge
	sessionID := a.sessionID
	cwd := space.ProjectRoot
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

func (a *App) prompt(text string) {
	if a.bridge == nil || a.sessionID == "" {
		return
	}
	client := a.bridge
	sessionID := a.sessionID
	state := a.conversationFor(sessionID)
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 30*time.Second)
		defer cancel()
		if err := client.Prompt(ctx, sessionID, text); err != nil {
			a.update(func() { state.ActionErr = err.Error() })
		}
	}()
}

func (a *App) abortAgent() {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	sessionID := a.sessionID
	state := a.conversationFor(sessionID)
	go func() {
		ctx, cancel := context.WithTimeout(a.bridgeCtx, 20*time.Second)
		defer cancel()
		if err := client.Abort(ctx, sessionID); err != nil {
			a.update(func() { state.ActionErr = err.Error() })
		}
	}()
}

func (a *App) stopAgent() {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	sessionID := a.sessionID
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

func (a *App) approve(id string, decision string) {
	if a.bridge == nil {
		return
	}
	client := a.bridge
	sessionID := a.sessionID
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
		a.hcHello = hc.HelloResult{}
		a.terminals = nil
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

// NewTerminal starts a shell in the selected Space's project root through
// herdr-core and opens it in a tab. Closing the tab detaches; it never stops
// the shell.
func (a *App) NewTerminal() {
	space, ok := a.space()
	if !ok {
		a.notice = "Create a Space before starting a terminal."
		return
	}
	client := a.hc
	if client == nil {
		a.notice = "herdr-core is not connected yet."
		a.refreshTerminals()
		return
	}
	sessionID := a.sessionID
	spaceID, cwd := space.ID, space.ProjectRoot
	go func() {
		if sessionID == "" {
			session, err := a.store.CreateSession(spaceID, "Terminal")
			if err != nil {
				a.update(func() { a.notice = "session: " + err.Error() })
				return
			}
			sessionID = session.ID
			a.update(func() {
				a.sessionID = sessionID
				a.layout = domain.SessionLayout{}
				a.reload()
			})
		}
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
				a.notice = "terminal: " + err.Error()
				return
			}
			a.notice = ""
			a.addTerminalTabTo(sessionID, result.Address)
			a.refreshTerminals()
		})
	}()
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
	form.Busy = true
	form.Err = ""
	title, root := form.Title, form.Root
	go func() {
		space, err := a.store.CreateSpace(title, root)
		a.update(func() {
			form.Busy = false
			if err != nil {
				form.Err = err.Error()
				return
			}
			form.Open = false
			form.Title, form.Root = "", ""
			a.spaceID = space.ID
			a.sessionID = ""
			a.reload()
		})
	}()
}

func (a *App) newSessionSubmit() {
	form := &a.newSession
	if form.Busy || a.spaceID == "" {
		return
	}
	form.Busy = true
	form.Err = ""
	title := form.Title
	go func() {
		session, err := a.store.CreateSession(a.spaceID, title)
		a.update(func() {
			form.Busy = false
			if err != nil {
				form.Err = err.Error()
				return
			}
			form.Open = false
			form.Title = ""
			a.sessionID = session.ID
			a.layout = domain.SessionLayout{}
			a.activeTab = ""
			a.activePane = ""
			a.reload()
		})
	}()
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

// repoFor returns a cached repository handle for a Session's project root.
func (a *App) repoFor(sessionID string) (*repository.Repo, string) {
	if repo, ok := a.repos[sessionID]; ok {
		return repo, ""
	}
	if message, ok := a.repoErr[sessionID]; ok {
		return nil, message
	}
	session, ok := a.sessionByID(sessionID)
	if !ok {
		return nil, "Session not found."
	}
	space, ok := a.spaceByID(session.SpaceID)
	if !ok {
		return nil, "Space not found."
	}
	repo, err := repository.Open(space.ProjectRoot, repository.Compact())
	if err != nil {
		a.repoErr[sessionID] = err.Error()
		return nil, err.Error()
	}
	a.repos[sessionID] = repo
	return repo, ""
}

// sessionCount is how many Sessions a Space holds, for the sidebar.
func (a *App) sessionCount(spaceID string) int {
	count := 0
	for _, session := range a.meta.Sessions {
		if session.SpaceID == spaceID {
			count++
		}
	}
	return count
}

func (a *App) sessionByID(id string) (domain.Session, bool) {
	for _, session := range a.meta.Sessions {
		if session.ID == id {
			return session, true
		}
	}
	return domain.Session{}, false
}

// openSessionAgent selects the Agent's Session and brings its conversation into view.
func (a *App) openSessionAgent(sessionID string) {
	session, ok := a.sessionByID(sessionID)
	if !ok {
		return
	}
	a.spaceID = session.SpaceID
	a.sessionID = session.ID
	a.layout = domain.SessionLayout{}
	a.activeTab = ""
	a.activePane = ""
	a.reload()
	a.focusChat()
}

func (a *App) focusChat() {
	for _, tab := range a.layout.Tabs {
		for _, leaf := range tab.Root.Leaves() {
			if leaf.Surface == domain.SurfaceChat {
				a.selectTab(tab.ID)
				a.activePane = leaf.ID
				return
			}
		}
	}
	a.addTab(domain.SurfaceChat)
}

func (a *App) spaceByID(id string) (domain.Space, bool) {
	for _, space := range a.meta.Spaces {
		if space.ID == id {
			return space, true
		}
	}
	return domain.Space{}, false
}

func (a *App) currentRepo() (*repository.Repo, string) {
	return a.repoFor(a.sessionID)
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

func (a *App) bindTerminal(address string) {
	agent := domain.AgentRun{
		SessionID:       a.sessionID,
		CoreID:          a.hcHello.Core.ID,
		TerminalAddress: address,
		Adapter:         "terminal",
	}
	if agent.CoreID == "" {
		a.notice = "The core identity is unknown; binding needs a live hello."
		return
	}
	run, err := a.store.BindAgent(agent.SessionID, agent.CoreID, agent.TerminalAddress, agent.Adapter, "")
	if err != nil {
		a.notice = err.Error()
		return
	}
	a.reload()
	a.attachAgentRun(run)
}

func (a *App) agentLabel(run domain.AgentRun) string {
	return run.Adapter + " · " + run.TerminalAddress
}

func (a *App) agentRun(id string) (domain.AgentRun, bool) {
	for _, agent := range a.meta.Agents {
		if agent.ID == id {
			return agent, true
		}
	}
	return domain.AgentRun{}, false
}

// attachAgentRun opens a live attachment for a bound run in the active pane.
func (a *App) attachAgentRun(run domain.AgentRun) {
	pane := a.paneFor(a.activePane)
	if pane == nil {
		return
	}
	pane.agentRunID = run.ID
	pane.term = terminal.New(run.TerminalAddress, a.agentLabel(run), false)
	pane.attachStarted = false
	pane.term.Err = ""
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
				state.Err = err.Error()
				state.Connecting = false
				p.attachStarted = false
			})
			return
		}
		a.update(func() {
			if err := state.Attach(attachment); err != nil {
				a.notice = err.Error()
			}
		})
	}()
}

// NewTab opens a conversation tab.
func (a *App) NewTab() { a.addTab(domain.SurfaceChat) }

// OpenSettings opens the settings sheet.
func (a *App) OpenSettings() {
	a.settings.Open = true
	a.loadRemoteStatus()
	a.loadPlugins()
}

// SplitRight opens a terminal pane to the right of the active pane.
func (a *App) SplitRight() { a.splitPane(domain.SplitRow, domain.SurfaceTerm) }

// SplitDown opens a file pane below the active pane.
func (a *App) SplitDown() { a.splitPane(domain.SplitColumn, domain.SurfaceFiles) }

// CloseActivePane closes the focused pane without touching any process.
func (a *App) CloseActivePane() { a.closePane(a.activePane) }

// NewSession opens the new-Session form for the selected Space.
func (a *App) NewSession() {
	a.newSession.Open = true
	a.newSession.Err = ""
}
