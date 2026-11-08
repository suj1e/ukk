# Product requirements — ukk

> Full product vision, not a feature-limited MVP. Delivery is phased; scope is retained here even if not in the first release. This is a greenfield reset: there is no data or state migration from the earlier Rust/GPUI prototype.

## Promise

**One workspace for many coding agents, on many machines.** Use a native desktop interface to run and coordinate agents. Use a native phone companion to monitor, message, approve and — only when needed — pop open a small floating live view of the machine.

Primary user: a developer who works with several coding agents and wants persistence, parallelism, native UI and reliable mobility.

## Current delivery boundary

Build a **complete product**, not a terminal MVP, on native Desktop (macOS, Windows, Linux) and iOS/iPadOS first:

- **Desktop:** native Go application built with MyGo, using MyGo's native UI. No webview-based primary interface, no GPUI/Rust application, and no TUI-themed desktop facade.
- **Runtime:** herdr-core v0.1.18 remains the external authority for persistent terminals and processes. ukk never copies or rewrites it.
- **Background service:** `ukk-bridge` authorizes remote, agent-adapter and screen connections and owns ukk's durable product state. Closing any ukk UI never stops an Agent.
- **Remote:** a self-hostable `ukk-relay` (WSS/TLS, operator authentication) plus independent STUN/TURN media connectivity. The relay never carries raw video.
- **Mobile:** native SwiftUI iOS/iPadOS companion with real QR/camera pairing, chat, approvals and APNs.

The plugin architecture, multi-Agent management, persistent state, safe device pairing, remote mobile session access and optional floating Remote Screen are core scope.

Android/Web/TUI are explicitly **deferred client implementations**. Do not build their UI, bootstrap repositories or claim shipping support during the current delivery track. Keep the protocol modular so they can be added later without reworking the bridge or runtime.

## Design pillars

1. **Multi-agent, not fixed pipeline.** omp is the first real provider; Codex, Claude Code and compatible agents are dynamic peers through one provider-neutral contract. Agents may delegate to children; no hard-coded Planner/Coder/Reviewer/Tester funnel.
2. **Spaces organize work.** Projects/worktrees/remote machines can contain sessions. Tabs and panes are view composition; closing a pane must not silently kill an agent.
3. **Native-first.** MyGo native UI on desktop, SwiftUI on iOS. The desktop shell is native Go, not an embedded browser dressed as a desktop app.
4. **Portable ongoing sessions.** Agent execution and terminals live in herdr-core, not in a window. Closing a pane, tab, window or the whole Desktop detaches the view. Reattach after app exit or network loss; do not promise crash recovery that the runtime does not provide.
5. **Extensible by design.** A small, reviewed plugin surface with lifecycle, capabilities, typed commands/events, versioning and per-device grants. Third-party plugin safety and durable grants are product scope, not a generic framework project.
6. **Remote Screen is a tool, not a mode.** A floating preview within the mobile session, tap to expand to a touch-control view, minimize back into the conversation. View and control are separate grants.

## Complete feature inventory

| Area | Intended capabilities |
|---|---|
| Workspaces | Multiple local/remote devices; named Spaces; project roots; recent/pinned; per-Space settings; quick switch and search. |
| Sessions | Multiple concurrent sessions, rename/archive/reopen, persistent history, timeline, interruption recovery, share a link to a specific workspace view. |
| Agents | Run omp first through a neutral provider contract; target Codex/Claude-compatible CLIs; status and child hierarchy; dispatch instructions; monitor parallel tasks; per-agent capabilities; human approval. |
| Composition | Tabs, resizable split panes, workspace presets, agent chat, terminal, files, diff, activity, search and command palette. |
| Code context | Git/worktree integration, changed files and hunks, branch metadata, read-only remote diff, open file, patch review, optional git actions behind permissions. |
| Structured UX | Turns, plans, tool calls, progress, results, permissions; provider-specific adapters with explicit capability detection and raw terminal fallback. Never derive approvals or tool events by parsing ANSI. |
| Remote mobile | QR pairing, trusted devices, live session streaming, continue chatting, approvals, push notifications, offline/reconnect, multi-host switching. |
| Remote Screen plugin | Floating live preview; tap to expand; mouse/touch/keyboard input only on grant; minimize, disconnect, screen selection; no default always-on capture. |
| Extensions | Plugin install/update/disable; registry; manifest and SDK; UI slots; commands; permissions; events; capability-mediated bridge RPC; plugin error recovery; OS sandbox where available. |
| Automation | Scheduled jobs, event triggers, webhooks, user-reviewed actions, agent handoffs and integrations as optional plugins. |
| Operations | Background bridge lifecycle, resource usage, logging, diagnostics, version negotiation for current clients, offline state, update checks, backups/export. |
| Security | Device-level grants, short-lived pairing, key rotation, permissions for approvals/screen/input, audit log, session termination, revocation, local-only mode. |
| Distribution | Unsigned macOS `.dmg`, Windows NSIS `.exe`, Linux `.deb` plus tarball, and an unsigned device-build iOS `.ipa`. Signing, notarization and App Store distribution are out of scope. |

Platform scope: **active** Desktop on macOS/Windows/Linux with one MyGo native-UI codebase, and SwiftUI iOS/iPadOS. **Deferred:** Android Compose, Web, Ratatui TUI — no work scheduled yet. A headless bridge is a runtime/deployment capability, not a separate UI client.

## UX flows

### Desktop everyday usage

Launch app → choose Space → open an existing Session or create one → start one or more Agents → switch between their tabs/panes → review changes, approvals and activity → close window; agents stay running → reopen and reattach.

### Mobile everyday usage

Open iOS App → select a paired device and Space → see live Agents/Sessions → tap a Session → compose a follow-up prompt or approve → optional floating screen control to interact with a native dialog → minimize screen and continue chat.

### First pairing

Desktop: Devices → Add iPhone → QR → verify and confirm the request. iPhone: Add Device → scan → show verification challenge → wait for desktop confirmation → store trusted device key → connect and subscribe.

Pairing grants no agent control, screen viewing or screen control by itself. Those are separate grants with their own consent and revocation.

## Navigation principles

Desktop: left sidebar Spaces / Agents / Sessions / Git; center native conversation or multi-pane view; optional right context panel; terminal present but not primary. iOS: Workspace / Sessions / Agents; session detail is chat-first. Remote screen must remain an overlay or compact floating window, not a new permanent navigation tab.

## Quality requirements

- No lost running agents when the Desktop UI closes or crashes. Closing a view detaches; only an explicit user action stops execution.
- Idempotent commands, resubscribe/replay after disconnect, and correct visibility of offline/stale state.
- Multi-client observation with explicit single-writer arbitration for sensitive control streams.
- Terminal provenance is explicit in the contract: human input and automatic Agent input must be source-tagged on the same write path, never inferred by sniffing bytes. The current MyGo plugin exposes one write path and uses herdr-core's legacy DATA path for both; observers do not write or resize, and backpressure is bounded. Source tagging remains an implementation requirement.
- Long Markdown must render incrementally and bounded; do not assume an unresolved upstream long-document issue is fixed.
- Accessibility, Dynamic Type where applicable, keyboard shortcuts, dark mode and reduced motion support.
- No outbound remote access, mic/audio capture or screen capture until the user enables and authorizes it.
- Screen sharing must visibly indicate active capture and expose a one-tap stop control.
- Responsive local navigation even when the bridge or network is unavailable.

## Distribution and acceptance artifacts

The final release candidates are **unsigned** and must be labelled as such:

| Platform | Artifact | Contents |
| --- | --- | --- |
| macOS | `.dmg` | unsigned `ukk.app` (ad-hoc signing allowed), bridge, README/licence, install notes |
| Windows | NSIS `.exe` | Desktop, bridge, Start Menu entry, uninstaller |
| Linux | `.deb` plus tarball | Desktop, bridge, `.desktop` entry, uninstall metadata |
| iOS | `.ipa` | device build of `UkkCompanion.app`, no signing |

An unsigned IPA is an artifact, not an installable App Store/TestFlight release. It requires later signing and a device provisioning path. Signing, notarization and store distribution are excluded from v1 acceptance; a physical-device run is a separate prerequisite gate that must not be fabricated.

## Core success scenario

On Mac, start two coding agents in the same Space; one creates a child agent. Close the Desktop window. On an iPhone that was QR-paired previously, reopen the same session, see the three processes and their correct states, send a message to a chosen Agent, inspect a diff, approve a request, briefly open an on-demand floating Remote Screen to handle a native dialog, then minimize it and continue the session. All state survives app reconnect; the relay never sees screen media.

## Reset scope

- No import, migration or compatibility layer for the previous Rust/GPUI Host, SQLite schema, protocol or release artifacts. The old `main` (including `f033cf0`) is historical reference only.
- No dummy `main`, no-op package, fake adapter, placeholder host or green-looking empty test may stand in for a milestone.
- Source presence is not completion; each milestone in [mygo-implementation](goals/mygo-implementation.md) has an evidence gate.

## Not in scope

A generic personal AI chatbot, mandatory cloud login, GPU-streamed desktop as the app's main navigation, a fixed multi-agent workflow engine, reimplementing herdr-core's terminal/runtime in ukk, or migrating users/data from the superseded Rust prototype.
