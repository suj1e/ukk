package app

import (
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/egoist/mygo/ui"
	"github.com/suj1e/ukk/apps/desktop/internal/agentview"
	"github.com/suj1e/ukk/apps/desktop/internal/bridge"
	"github.com/suj1e/ukk/apps/desktop/internal/corelayout"
	"github.com/suj1e/ukk/apps/desktop/internal/extensions"
	"github.com/suj1e/ukk/apps/desktop/internal/ui/kit"
)

// View renders the whole window: spaces and agents in the sidebar, then the
// tabs and panes of the active space's herdr-app document.
func (a *App) View(c *ui.Context) {
	if c.Theme().Dark != a.themeDark {
		a.themeDark = c.Theme().Dark
		a.theme = kit.Theme(a.themeDark)
	}
	c.SetTheme(a.theme)
	ui.Row(c).Fill().Background(c.Theme().Background).Children(func() {
		a.sidebar(c)
		ui.Column(c).Grow(1).FillHeight().Children(func() {
			if space, ok := a.activeSpace(); ok {
				activeID := ""
				if tab, ok := a.activeTab(); ok {
					activeID = tab.ID
				}
				a.tabBar(c, space, activeID)
			}
			ui.Column(c).Grow(1).Children(func() {
				a.body(c)
			})
			a.statusBar(c)
		})
	})
	a.newSpaceDialog(c)
	a.renameDialog(c)
	a.settingsDialog(c)
	a.paletteDialog(c)
}

func (a *App) body(c *ui.Context) {
	if a.world == nil {
		if a.hcErr != "" {
			if kit.Empty(c, "herdr-core is unavailable", firstLine(a.hcErr), "Retry") {
				a.hcErr = ""
				a.refreshTerminals()
			}
			return
		}
		if a.worldErr != "" {
			if kit.Empty(c, "The shared world is unavailable", firstLine(a.worldErr), "Retry") {
				a.retryWorld()
			}
			return
		}
		kit.Meta(c, "Loading the shared world…")
		return
	}
	if _, ok := a.activeSpace(); !ok {
		if kit.Empty(c, "No Space yet",
			"A Space is a project folder. Tabs open shells and Agent conversations inside it.",
			"New Space") {
			a.NewSpace()
		}
		return
	}
	if a.notice != "" {
		if kit.Banner(c, "warn", a.notice, "Dismiss") {
			a.notice = ""
		}
	}
	a.renderPanes(c)
}

// newTabMenu offers the surfaces a new tab can show.
func (a *App) newTabMenu(m *ui.Menu) {
	if m.Item("Shell").Chosen() {
		a.NewTerminal()
	}
	if m.Item("Chat").Chosen() {
		a.newConversationTab()
	}
	if m.Item("Files").Chosen() {
		a.newModTab("files", "Files")
	}
	if m.Item("Diff").Chosen() {
		a.newModTab("changes", "Diff")
	}
}

// tabBar shows the tabs of the active space and the world actions beside them.
func (a *App) tabBar(c *ui.Context, space corelayout.SharedSpace, activeID string) {
	theme := c.Theme()
	ui.Row(c).FillWidth().Height(42).PaddingX(10).Gap(3).
		AlignItems(ui.Center).Background(theme.Surface).
		BorderWidth(0, 0, 1, 0).BorderColor(theme.Border).
		Role(ui.RoleTabList).Children(func() {
		for _, tab := range space.Tabs {
			tab := tab
			name := tabDisplayName(tab)
			selected := tab.ID == activeID
			row := ui.Row(c).Gap(6).AlignItems(ui.Center).
				Height(29).PaddingX(10).Radius(7).
				Role(ui.RoleTab).AccessSelected(selected)
			if selected {
				row.Background(theme.SurfaceHover).Border(1, theme.Border)
			}
			row.Children(func() {
				label := ui.ButtonBase(c).Label(name)
				label.Role(ui.RoleTab).AccessSelected(selected)
				label.Children(func() {
					ui.Text(c, name).FontSize(12).
						TextColor(boolColor(selected, theme.Text, theme.TextMuted))
				})
				if label.Clicked() && !selected {
					a.selectTab(tab.ID)
				}
				close := ui.ButtonBase(c).Label("Close "+name).
					Size(18, 18).Radius(4)
				close.Children(func() {
					ui.Text(c, "×").FontSize(13).TextColor(theme.TextMuted)
				})
				if close.Clicked() {
					a.closeTab(tab.ID)
				}
			})
		}
		ui.Spacer(c)
		ui.PopoverMenuButton(c, "+", a.newTabMenu).Label("New tab").
			PaddingX(7).PaddingY(3).FontSize(13)
		if tab, ok := a.activeTab(); ok {
			active := tab
			ui.PopoverMenuButton(c, "···", func(m *ui.Menu) {
				if m.Item("Rename tab…").Chosen() {
					a.openTabRename(space, active)
				}
				if m.Item("Close tab").Chosen() {
					a.closeTab(active.ID)
				}
			}).Label("Tab actions").PaddingX(6).PaddingY(3).FontSize(12)
		}
		a.tabActions(c)
	})
}

// tabActions are the world actions beside the tab strip.
func (a *App) tabActions(c *ui.Context) {
	ui.PopoverMenuButton(c, "Split", func(m *ui.Menu) {
		if m.Item("Split right").Chosen() {
			a.splitActive(corelayout.TypeRow)
		}
		if m.Item("Split down").Chosen() {
			a.splitActive(corelayout.TypeColumn)
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
				spaces := a.spaces()
				activeID := ""
				if active, ok := a.activeSpace(); ok {
					activeID = active.ID
				}
				if len(spaces) == 0 {
					if a.world == nil {
						kit.Meta(c, "The shared world is not loaded.")
					} else {
						kit.Meta(c, "No Space yet.")
					}
				}
				for _, space := range spaces {
					space := space
					name := spaceDisplayName(space)
					selected := activeID == space.ID
					ui.Row(c).FillWidth().Gap(2).AlignItems(ui.Center).Children(func() {
						button := ui.ButtonBase(c).Grow(1)
						button.Children(func() {
							ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Padding(4, 7).Radius(6).
								Background(boolColor(selected, theme.SurfaceHover, ui.Transparent)).Children(func() {
								ui.Box(c).Size(18, 18).Radius(4).Background(spaceTint(name)).
									Center().Children(func() {
									ui.Text(c, initial(name)).FontSize(9).FontWeight(700).
										TextColor(theme.InverseText)
								})
								ui.Column(c).Grow(1).Gap(1).Children(func() {
									ui.Text(c, name).FontSize(12).FontWeight(600).NoWrap().
										Ellipsis(name)
									if space.Folder != "" {
										ui.Text(c, space.Folder).FontSize(9).TextColor(theme.TextMuted).
											NoWrap().Ellipsis(space.Folder)
									}
								})
								if count := len(space.Tabs); count > 0 {
									ui.Text(c, strconv.Itoa(count)).FontSize(9).TextColor(theme.TextMuted)
								}
							})
						})
						if button.Clicked() {
							a.selectSpace(space.ID)
						}
						ui.PopoverMenuButton(c, "···", func(m *ui.Menu) {
							if m.Item("Rename space…").Chosen() {
								a.openSpaceRename(space)
							}
							if m.Item("Close space").Chosen() {
								a.closeSpace(space.ID)
							}
						}).Label("Space actions").FontSize(11).PaddingX(4).PaddingY(2)
					})
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
				a.OpenSettings()
			}
			if ui.Button(c, "+ Space").FillWidth().Height(28).FontSize(12).Clicked() {
				a.newSpace.Open = true
				a.newSpace.Err = ""
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
					a.openTerminal(agent.Address)
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
						if name := a.tabTitleForSession(snapshot.SessionID); name != "" {
							title = name
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

// tabDisplayName is the tab's name, or a neutral label when it has none.
func tabDisplayName(tab corelayout.SharedTab) string {
	if name := strings.TrimSpace(tab.Name); name != "" {
		return name
	}
	return "Tab"
}

// spaceDisplayName is the space's name, or its folder's base name when the
// document has not named it.
func spaceDisplayName(space corelayout.SharedSpace) string {
	if name := strings.TrimSpace(space.Name); name != "" {
		return name
	}
	if space.Folder != "" {
		return filepath.Base(space.Folder)
	}
	return "Space"
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

func (a *App) newSpaceDialog(c *ui.Context) {
	if !a.newSpace.Open {
		return
	}
	form := &a.newSpace
	ui.Modal(c, &form.Open, func() {
		ui.Column(c).Width(460).Gap(12).Padding(20).Children(func() {
			kit.PageTitle(c, "New Space")
			ui.Text(c, "A Space is a project folder. Its first tab opens a shell there; Agent conversations are tabs inside it.").
				FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
			ui.Field(c, "Name", func() {
				ui.TextInput(c, &form.Name).Placeholder("ukk").FillWidth()
			})
			ui.Field(c, "Project folder", func() {
				ui.TextInput(c, &form.Folder).Placeholder("/Users/you/project").FillWidth()
			})
			kit.ErrorLine(c, form.Err)
			ui.Row(c).Gap(8).Justify(ui.End).Children(func() {
				if ui.Button(c, "Cancel").Clicked() {
					form.Open = false
				}
				if ui.PrimaryButton(c, "Create").Disabled(form.Busy ||
					strings.TrimSpace(form.Name) == "" || strings.TrimSpace(form.Folder) == "").
					Clicked() {
					a.newSpaceSubmit()
				}
			})
		})
	})
}

func (a *App) renameDialog(c *ui.Context) {
	if !a.rename.Open {
		return
	}
	form := &a.rename
	title := "Rename space"
	if form.Kind == "tab" {
		title = "Rename tab"
	}
	ui.Modal(c, &form.Open, func() {
		ui.Column(c).Width(420).Gap(12).Padding(20).Children(func() {
			kit.PageTitle(c, title)
			ui.Field(c, "Name", func() {
				ui.TextInput(c, &form.Name).FillWidth()
			})
			kit.ErrorLine(c, form.Err)
			ui.Row(c).Gap(8).Justify(ui.End).Children(func() {
				if ui.Button(c, "Cancel").Clicked() {
					form.Open = false
				}
				if ui.PrimaryButton(c, "Rename").Disabled(strings.TrimSpace(form.Name) == "").Clicked() {
					a.renameSubmit()
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
		ui.Text(c, "ukk keeps Agent processes in the background bridge and terminals in herdr-core. Closing this view detaches metadata; it does not stop them.").
			FontSize(11).TextColor(c.Theme().TextMuted).Wrap()
	})
}
