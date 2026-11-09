package app

import (
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/agentview"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
	"github.com/suj1e/ukk/apps/desktop/internal/hc"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
	uispace "github.com/suj1e/ukk/apps/desktop/internal/ui/workspace"
	domain "github.com/suj1e/ukk/apps/desktop/internal/workspace"
)

// View renders the whole window: a spaces and agents sidebar, then the tabs
// and panes of the selected Session.
func (a *App) View(c *ui.Context) {
	if c.Theme().Dark != a.themeDark {
		a.themeDark = c.Theme().Dark
		a.theme = kit.Theme(a.themeDark)
	}
	c.SetTheme(a.theme)
	ui.Row(c).Fill().Background(c.Theme().Background).Children(func() {
		a.sidebar(c)
		ui.Column(c).Grow(1).FillHeight().Children(func() {
			if a.sessionID != "" {
				uispace.TabBar(c, a.layout.Tabs, a.activeTab, a.selectTab, a.closeTab, a.NewTerminal, a.newTabMenu, a.tabActions)
			}
			ui.Column(c).Grow(1).Children(func() {
				if a.sessionID == "" {
					if kit.Empty(c, "No Space yet",
						"A Space is a project root. Tabs open shells and Agent sessions inside it.",
						"New Space") {
						a.newSpace.Open = true
					}
					return
				}
				if a.notice != "" {
					if kit.Banner(c, "warn", a.notice, "Dismiss") {
						a.notice = ""
					}
				}
				a.renderPanes(c)
			})
			a.statusBar(c)
		})
	})
	a.newSpaceDialog(c)
	a.newSessionDialog(c)
	a.settingsDialog(c)
	a.paletteDialog(c)
}

// newTabMenu offers the surfaces a new tab can show.
func (a *App) newTabMenu(m *ui.Menu) {
	if m.Item("Shell").Chosen() {
		a.NewTerminal()
	}
	if m.Item("Chat").Chosen() {
		a.addTab(domain.SurfaceChat)
	}
	if m.Item("Files").Chosen() {
		a.addTab(domain.SurfaceFiles)
	}
	if m.Item("Diff").Chosen() {
		a.addTab(domain.SurfaceDiff)
	}
}

// tabActions are the workspace actions beside the tab strip.
func (a *App) tabActions(c *ui.Context) {
	if ui.Button(c, "+ Chat").FontSize(11).Clicked() {
		a.addTab(domain.SurfaceChat)
	}
	ui.PopoverMenuButton(c, "Split", func(m *ui.Menu) {
		for _, direction := range []struct {
			label string
			value string
		}{{"Split right", domain.SplitRow}, {"Split down", domain.SplitColumn}} {
			direction := direction
			m.Submenu(direction.label, func(sub *ui.Menu) {
				for _, surface := range []string{domain.SurfaceChat, domain.SurfaceTerm, domain.SurfaceFiles, domain.SurfaceDiff, domain.SurfaceLog} {
					surface := surface
					if sub.Item(surfaceTitle(surface)).Chosen() {
						a.splitPane(direction.value, surface)
					}
				}
			})
		}
	}).PaddingX(8).PaddingY(3).FontSize(11)
	if ui.Button(c, "Palette").Label("Command palette").PaddingX(7).PaddingY(3).FontSize(11).Clicked() {
		a.OpenPalette()
	}
}

func (a *App) sidebar(c *ui.Context) {
	theme := c.Theme()
	ui.Column(c).Width(268).FillHeight().Background(theme.Surface).
		BorderWidth(0, 1, 0, 0).BorderColor(theme.Border).Children(func() {
		ui.Row(c).FillWidth().Height(46).PaddingX(14).Gap(8).AlignItems(ui.Center).Children(func() {
			ui.Text(c, "ukk").FontSize(14).FontWeight(600)
			ui.Spacer(c)
			if ui.Button(c, "Palette").Label("Command palette").PaddingX(6).PaddingY(2).FontSize(10).Clicked() {
				a.OpenPalette()
			}
		})

		ui.Scroll(c).Grow(1).MinHeight(90).Children(func() {
			ui.Column(c).Gap(1).PaddingX(8).PaddingY(4).Children(func() {
				kit.SectionLabel(c, "Spaces")
				for _, space := range a.meta.Spaces {
					space := space
					selected := space.ID == a.spaceID
					button := ui.ButtonBase(c).FillWidth()
					button.Children(func() {
						ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Padding(4, 7).Radius(6).
							Background(boolColor(selected, theme.SurfaceHover, ui.Transparent)).Children(func() {
							ui.Box(c).Size(18, 18).Radius(4).Background(spaceTint(space.Title)).
								Center().Children(func() {
								ui.Text(c, initial(space.Title)).FontSize(9).FontWeight(700).
									TextColor(theme.InverseText)
							})
							ui.Column(c).Grow(1).Gap(1).Children(func() {
								ui.Text(c, space.Title).FontSize(12).FontWeight(600).NoWrap().
									Ellipsis(space.Title)
								ui.Text(c, space.ProjectRoot).FontSize(9).TextColor(theme.TextMuted).
									NoWrap().Ellipsis(space.ProjectRoot)
							})
							if count := a.sessionCount(space.ID); count > 0 {
								ui.Text(c, strconv.Itoa(count)).FontSize(9).TextColor(theme.TextMuted)
							}
						})
					})
					if button.Clicked() {
						a.spaceID = space.ID
						a.sessionID = ""
						a.layout = domain.SessionLayout{}
						a.activeTab = ""
						a.activePane = ""
						a.pickSelection()
						a.reload()
					}
					for _, session := range a.meta.Sessions {
						if session.SpaceID != space.ID {
							continue
						}
						session := session
						indicator := theme.TextMuted
						switch session.Lifecycle {
						case domain.LifecycleActive:
							indicator = theme.Success
						case domain.LifecyclePaused:
							indicator = theme.Warning
						}
						ui.Column(c).PaddingStart(14).Children(func() {
							if uispace.SessionRow(c, session.Title, session.Lifecycle, indicator, session.ID == a.sessionID) {
								a.sessionID = session.ID
								a.layout = domain.SessionLayout{}
								a.activeTab = ""
								a.activePane = ""
								a.reload()
							}
						})
					}
				}
			})
		})

		ui.Scroll(c).Grow(1).MinHeight(90).Children(func() {
			ui.Column(c).Gap(1).PaddingX(8).PaddingY(4).Children(func() {
				a.agentSidebar(c)
			})
		})

		ui.Column(c).FillWidth().Height(69).Padding(8, 4).Gap(0).BorderWidth(1, 0, 0, 0).
			BorderColor(theme.Border).Children(func() {
			if ui.Button(c, "Settings").FillWidth().Height(28).FontSize(12).Clicked() {
				a.settings.Open = true
				a.loadRemoteStatus()
			}
			if ui.Button(c, "+ Space").FillWidth().Height(28).FontSize(12).Clicked() {
				a.newSpace.Open = true
			}
		})
	})
}

// statusBar is the 28px footer Herdr draws under the panes.
func (a *App) statusBar(c *ui.Context) {
	theme := c.Theme()
	agents := agentview.FromTerminals(a.terminals, time.Now())
	needs := 0
	for _, agent := range agents {
		if agent.State == "blocked" || agent.State == "error" {
			needs++
		}
	}
	ui.Row(c).FillWidth().Height(28).PaddingX(14).Gap(16).AlignItems(ui.Center).
		Background(theme.Surface).BorderWidth(1, 0, 0, 0).BorderColor(theme.Border).
		Children(func() {
			kit.Dot(c, a.herdrColor())
			ui.Text(c, "This computer").FontSize(11).TextColor(theme.TextMuted)
			ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
				kit.Dot(c, theme.TextMuted)
				ui.Text(c, kit.Plural(len(agents), "agent", "agents")).FontSize(11).
					TextColor(theme.TextMuted)
				if needs > 0 {
					ui.Text(c, "· "+strconv.Itoa(needs)+" needs you").FontSize(11).
						TextColor(theme.Danger)
				}
			})
			ui.Spacer(c)
			ui.Text(c, "Ctrl+Shift+P  commands").FontSize(11).TextColor(theme.TextMuted)
		})
}

// agentSidebar draws terminal Agents grouped by attention, using the shared
// agentview projection, then the bridge's structured provider sessions.
func (a *App) agentSidebar(c *ui.Context) {
	theme := c.Theme()
	agents := agentview.FromTerminals(a.terminals, time.Now())
	if len(agents) > 0 {
		kit.SectionLabel(c, "Agents")
		for _, section := range agentview.Sections(agents) {
			ui.Text(c, section.Name).FontSize(9).TextColor(theme.TextMuted).Padding(4, 7)
			for _, agent := range section.Agents {
				agent := agent
				button := ui.ButtonBase(c).FillWidth()
				button.Children(func() {
					ui.Row(c).FillWidth().Gap(7).AlignItems(ui.Center).Padding(4, 7).Radius(5).Children(func() {
						kit.Dot(c, agentStateColor(agent.State, theme))
						ui.Column(c).Grow(1).Gap(0).Children(func() {
							ui.Row(c).Gap(5).AlignItems(ui.Center).Children(func() {
								ui.Text(c, agent.App).FontSize(10).FontWeight(600).TextColor(theme.TextMuted)
								ui.Text(c, agent.Title).FontSize(11).NoWrap().Ellipsis(agent.Title)
							})
							if agent.Place != "" {
								ui.Text(c, agent.Place).FontSize(9).TextColor(theme.TextMuted).
									NoWrap().Ellipsis(agent.Place)
							}
						})
						if agent.Elapsed != "" {
							ui.Text(c, agent.Elapsed).FontSize(9).TextColor(theme.TextMuted)
						}
					})
				})
				if button.Clicked() {
					a.openTerminal(hc.Terminal{Address: agent.Address})
				}
			}
		}
	}
	if len(a.liveAgents) > 0 {
		kit.SectionLabel(c, "Provider sessions")
		providers := make([]bridge.AgentSnapshot, 0, len(a.liveAgents))
		for _, snapshot := range a.liveAgents {
			providers = append(providers, snapshot)
		}
		sort.Slice(providers, func(i, j int) bool { return providers[i].UpdatedMS > providers[j].UpdatedMS })
		for _, snapshot := range providers {
			snapshot := snapshot
			button := ui.ButtonBase(c).FillWidth()
			button.Children(func() {
				ui.Row(c).FillWidth().Gap(7).AlignItems(ui.Center).Padding(4, 7).Radius(5).Children(func() {
					kit.Dot(c, agentColor(snapshot.State, theme))
					ui.Column(c).Grow(1).Gap(0).Children(func() {
						title := snapshot.Provider
						if session, ok := a.sessionByID(snapshot.SessionID); ok {
							title = session.Title
						}
						ui.Text(c, title).FontSize(11).NoWrap().Ellipsis(title)
						ui.Text(c, snapshot.Provider+" · "+snapshot.State).FontSize(9).
							TextColor(theme.TextMuted).NoWrap()
					})
					if elapsed := agentElapsed(snapshot); elapsed != "" {
						ui.Text(c, elapsed).FontSize(9).TextColor(theme.TextMuted)
					}
				})
			})
			if button.Clicked() {
				a.openSessionAgent(snapshot.SessionID)
			}
		}
	}
}

func agentStateColor(state string, theme *ui.Theme) ui.Color {
	switch state {
	case "working":
		return theme.Warning
	case "blocked", "error":
		return theme.Danger
	case "done":
		return theme.Success
	default:
		return theme.TextMuted
	}
}

func agentColor(state string, theme *ui.Theme) ui.Color {
	switch state {
	case "running":
		return theme.Warning
	case "blocked", "failed":
		return theme.Danger
	case "done":
		return theme.Success
	default:
		return theme.TextMuted
	}
}

// agentElapsed is how long the current provider turn has run, or "".
func agentElapsed(snapshot bridge.AgentSnapshot) string {
	for i := len(snapshot.Turns) - 1; i >= 0; i-- {
		turn := snapshot.Turns[i]
		if turn.State == "running" && turn.StartedMS > 0 {
			return kit.FormatDuration(time.Since(time.UnixMilli(turn.StartedMS)).Milliseconds())
		}
	}
	return ""
}

// spaceTint gives a Space a stable color, like the program tiles in Herdr.
func spaceTint(title string) ui.Color {
	colors := []string{"#E8912A", "#5AA9FF", "#4CC38A", "#C792EA", "#F85149", "#E3B341", "#56D4DD"}
	hash := uint32(2166136261)
	for _, r := range title {
		hash ^= uint32(r)
		hash *= 16777619
	}
	return ui.Hex(colors[int(hash)%len(colors)])
}

func initial(title string) string {
	for _, r := range title {
		return strings.ToUpper(string(r))
	}
	return "?"
}

func boolColor(on bool, yes, no ui.Color) ui.Color {
	if on {
		return yes
	}
	return no
}

func (a *App) bridgeColor() ui.Color {
	theme := a.theme
	if theme == nil {
		return ui.Transparent
	}
	if a.bridge != nil && a.bridgeErr == "" {
		return theme.Success
	}
	if a.bridgeErr != "" {
		return theme.Danger
	}
	return theme.Warning
}

func (a *App) bridgeLabel() string {
	switch {
	case a.bridge != nil && a.bridgeErr == "":
		return "bridge connected"
	case a.bridgeErr != "":
		return "bridge: " + firstLine(a.bridgeErr)
	default:
		return "bridge starting"
	}
}

func herdrLabel(connected bool, err string) string {
	if connected {
		return "herdr-core connected"
	}
	if err != "" {
		return "herdr-core: " + firstLine(err)
	}
	return "herdr-core not connected"
}

// herdrColor is the status dot for the terminal runtime.
func (a *App) herdrColor() ui.Color {
	theme := a.theme
	if theme == nil {
		return ui.Transparent
	}
	switch {
	case a.hc != nil:
		return theme.Success
	case a.hcErr != "":
		return theme.Danger
	default:
		return theme.Warning
	}
}

func firstLine(text string) string {
	if index := strings.IndexByte(text, '\n'); index >= 0 {
		return text[:index]
	}
	return text
}

func (a *App) selectTab(id string) {
	a.activeTab = id
	a.mutateLayout(func(layout *domain.SessionLayout) error {
		layout.ActiveTab = id
		return nil
	})
	if leaves := a.tabRoot().Leaves(); len(leaves) > 0 {
		a.activePane = leaves[0].ID
	}
}

func (a *App) newSpaceDialog(c *ui.Context) {
	if !a.newSpace.Open {
		return
	}
	form := &a.newSpace
	ui.Modal(c, &form.Open, func() {
		ui.Column(c).Width(460).Gap(12).Padding(20).Children(func() {
			kit.PageTitle(c, "New Space")
			ui.Text(c, "A Space is a project root. Sessions and Agents live inside it.").
				FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
			ui.Field(c, "Name", func() {
				ui.TextInput(c, &form.Title).Placeholder("ukk").FillWidth()
			})
			ui.Field(c, "Project root", func() {
				ui.TextInput(c, &form.Root).Placeholder("/Users/you/project").FillWidth()
			})
			kit.ErrorLine(c, form.Err)
			ui.Row(c).Gap(8).Justify(ui.End).Children(func() {
				if ui.Button(c, "Cancel").Clicked() {
					form.Open = false
				}
				if ui.PrimaryButton(c, "Create").Disabled(form.Busy ||
					strings.TrimSpace(form.Title) == "" || strings.TrimSpace(form.Root) == "").
					Clicked() {
					a.newSpaceSubmit()
				}
			})
		})
	})
}

func (a *App) newSessionDialog(c *ui.Context) {
	if !a.newSession.Open {
		return
	}
	form := &a.newSession
	ui.Modal(c, &form.Open, func() {
		ui.Column(c).Width(420).Gap(12).Padding(20).Children(func() {
			kit.PageTitle(c, "New Session")
			ui.Text(c, "A Session groups Agent runs, terminals, files and diffs. It never owns a process by itself.").
				FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
			ui.Field(c, "Title", func() {
				ui.TextInput(c, &form.Title).Placeholder("Implement the bridge").FillWidth()
			})
			kit.ErrorLine(c, form.Err)
			ui.Row(c).Gap(8).Justify(ui.End).Children(func() {
				if ui.Button(c, "Cancel").Clicked() {
					form.Open = false
				}
				if ui.PrimaryButton(c, "Create").Disabled(form.Busy || strings.TrimSpace(form.Title) == "").Clicked() {
					a.newSessionSubmit()
				}
			})
		})
	})
}

func (a *App) settingsDialog(c *ui.Context) {
	if !a.settings.Open {
		return
	}
	settings := &a.settings
	ui.Modal(c, &settings.Open, func() {
		ui.Column(c).Width(620).MaxHeight(640).Gap(12).Padding(20).Children(func() {
			ui.Row(c).Gap(10).AlignItems(ui.Center).Children(func() {
				kit.PageTitle(c, "Settings")
				ui.Spacer(c)
				ui.Tabs(c, &settings.Tab, "Remote access", "Plugins", "About")
			})
			switch settings.Tab {
			case 0:
				a.remoteSettings(c)
			case 1:
				a.pluginSettings(c)
			default:
				a.aboutSettings(c)
			}
			kit.ErrorLine(c, settings.Err)
			ui.Row(c).Justify(ui.End).Children(func() {
				if ui.Button(c, "Close").Clicked() {
					settings.Open = false
				}
			})
		})
	})
}

func (a *App) remoteSettings(c *ui.Context) {
	settings := &a.settings
	status := settings.Status
	ui.Column(c).Gap(10).Children(func() {
		kit.KeyValue(c, "Host ID", status.HostID)
		kit.KeyValue(c, "Relay", settingsRelayLabel(status))
		kit.KeyValue(c, "Bridge", a.bridgeLabel())
		ui.Row(c).Gap(8).Children(func() {
			if ui.PrimaryButton(c, "Create pairing invite").Disabled(settings.Busy || a.bridge == nil).Clicked() {
				a.createInvite()
			}
			if settings.Invite != nil && ui.Button(c, "Copy payload").Clicked() {
				c.WriteClipboard(settings.Invite.Payload)
				c.Toast("Pairing payload copied")
			}
		})
		if settings.Invite != nil {
			ui.Row(c).Gap(14).AlignItems(ui.Start).Children(func() {
				if settings.QR != nil {
					ui.Image(c, settings.QR).Size(180, 180)
				}
				ui.Column(c).Grow(1).Gap(4).Children(func() {
					ui.Text(c, "One-time invite").FontSize(12).FontWeight(600)
					kit.Meta(c, "Expires "+time.UnixMilli(settings.Invite.ExpireMS).Format("15:04:05"))
					ui.Text(c, "Scan from the iPhone app, then compare the six-digit code on both screens before confirming.").
						FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
					kit.Mono(c, trimPayload(settings.Invite.Payload))
				})
			})
		}
		kit.SectionLabel(c, "Paired devices")
		if len(status.Devices) == 0 {
			kit.Meta(c, "No device is paired.")
		}
		for _, device := range status.Devices {
			device := device
			ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Padding(6, 8).
				Radius(c.Theme().Radius).Background(c.Theme().Surface).
				Border(1, c.Theme().Border).Children(func() {
				ui.Column(c).Grow(1).Gap(1).Children(func() {
					ui.Text(c, device.Label).FontSize(12).FontWeight(600)
					kit.Meta(c, strings.Join(device.Scopes, ", "))
					kit.Mono(c, device.ID)
				})
				if device.Revoked {
					kit.Pill(c, "revoked", "danger")
				} else {
					ui.PopoverMenuButton(c, "Grant", func(m *ui.Menu) {
						if m.Item("Approve actions (60 min)").Chosen() {
							a.grantDevice(device.ID, "agent.approve", 60)
						}
						if m.Item("Screen view (30 min)").Chosen() {
							a.grantDevice(device.ID, "screen.view", 30)
						}
						if m.Item("Screen control (15 min)").Chosen() {
							a.grantDevice(device.ID, "screen.control", 15)
						}
					})
				}
				if !device.Revoked && settings.Revoke == device.ID {
					if ui.Button(c, "Confirm revoke").Clicked() {
						a.revokeDevice(device.ID)
					}
				} else if ui.Button(c, "Revoke").Clicked() {
					settings.Revoke = device.ID
				}
			})
		}
		if status.PairingCode != "" {
			ui.Column(c).FillWidth().Gap(6).Padding(10, 12).Radius(c.Theme().Radius).
				Background(c.Theme().Surface).Border(1, c.Theme().Warning).Children(func() {
				ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
					kit.Pill(c, "pairing", "warn")
					ui.Text(c, "Compare this code on the phone").FontSize(11)
					ui.Text(c, status.PairingCode).FontSize(20).FontWeight(700).LetterSpacing(4)
				})
				ui.Row(c).Gap(8).Children(func() {
					if ui.PrimaryButton(c, "Allow device").Clicked() {
						a.confirmPairing(true)
					}
					if ui.Button(c, "Deny").Clicked() {
						a.confirmPairing(false)
					}
				})
			})
		}
		ui.Row(c).Gap(8).Children(func() {
			ui.Button(c, "Refresh").OnClick(a.loadRemoteStatus)
			ui.Spacer(c)
		})
		if status.RelayURL == "" {
			kit.Banner(c, "warn", "No relay is configured. Local Desktop and bridge still work; a phone needs a relay URL and operator token in bridge.json.", "")
		}
	})
}

func settingsRelayLabel(status bridge.Status) string {
	if status.RelayURL == "" {
		return "not configured"
	}
	if status.RelayConnected {
		return status.RelayURL + " (connected)"
	}
	return status.RelayURL + " (offline)"
}

func trimPayload(payload string) string {
	if len(payload) <= 120 {
		return payload
	}
	return payload[:120] + "…"
}

func (a *App) pluginSettings(c *ui.Context) {
	settings := &a.settings
	ui.Column(c).Gap(8).Children(func() {
		ui.Text(c, "Plugins run out of process with declared capabilities. Installing or enabling code is an explicit local action.").
			FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
		ui.Row(c).Gap(8).AlignItems(ui.End).Children(func() {
			ui.Column(c).Grow(1).Children(func() {
				ui.Field(c, "Plugin directory", func() {
					ui.TextInput(c, &settings.PluginDir).Placeholder("/path/to/plugin").FillWidth()
				})
			})
			ui.Button(c, "Install").Disabled(settings.PluginBusy || settings.PluginDir == "").
				OnClick(a.installPlugin)
			ui.Button(c, "Refresh").OnClick(a.loadPlugins)
		})
		kit.ErrorLine(c, settings.PluginErr)
		if len(settings.Plugins) == 0 {
			kit.Meta(c, "No plugin is installed.")
		}
		for _, plugin := range settings.Plugins {
			plugin := plugin
			ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Padding(6, 8).
				Radius(c.Theme().Radius).Background(c.Theme().Surface).
				Border(1, c.Theme().Border).Children(func() {
				ui.Column(c).Grow(1).Gap(1).Children(func() {
					ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
						ui.Text(c, plugin.Name).FontSize(12).FontWeight(600)
						kit.Pill(c, plugin.State, plugin.State)
						kit.Pill(c, plugin.Trust, "")
					})
					kit.Mono(c, plugin.ID+" · "+plugin.Version)
				})
				if plugin.State == extensions.StateReady {
					ui.Button(c, "Disable").OnClick(func() { a.setPluginEnabled(plugin.ID, false) })
				} else {
					ui.Button(c, "Enable").OnClick(func() { a.setPluginEnabled(plugin.ID, true) })
				}
			})
		}
	})
}

func (a *App) aboutSettings(c *ui.Context) {
	ui.Column(c).Gap(6).Children(func() {
		kit.KeyValue(c, "Version", "0.1.0")
		kit.KeyValue(c, "Data", a.dir)
		kit.KeyValue(c, "Bridge", a.bridgeLabel())
		kit.KeyValue(c, "herdr-core", a.hcHello.Version)
		kit.KeyValue(c, "Core", a.hcHello.Core.ID)
		ui.Text(c, "ukk keeps Agent processes in the background bridge and terminals in herdr-core. Closing this window detaches views; it does not stop them.").
			FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
	})
}
