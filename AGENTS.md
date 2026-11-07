# Engineering instructions for coding agents working on ukk

Read [docs/OVERVIEW.zh-CN.md](docs/OVERVIEW.zh-CN.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/DOMAIN_MODEL.md](docs/DOMAIN_MODEL.md), [docs/PLUGINS.md](docs/PLUGINS.md) and [docs/SECURITY.md](docs/SECURITY.md) before implementing features.

## Non-negotiable product decisions

1. This is **Route B**: build a new Rust/GPUI Desktop. Do not fork or copy herdr-app UI.
2. Use herdr-core as the persistent Agent terminal/process runtime; do not rewrite that functionality.
3. iOS is native SwiftUI; do not replace with embedded WebView or force GPUI onto iOS.
4. Multi-agent is dynamic; never hard-code Planner/Coder/Reviewer/Tester as a workflow.
5. Space / ukk Session / AgentRun / TerminalRef / Pane are distinct entities with distinct lifecycles.
6. UI window closing must not terminate ongoing agents.
7. Remote Screen is a **small, optional floating session plugin** that expands temporarily, not a top-level mode or standalone product.
8. Pairing/authorization, Agent control and screen control are separate; no implicit access escalation.
9. Relay carries control/signalling, not raw video frames; screen media uses independent low-latency transport.
10. Prefer typed structured Agent events; never infer approvals/tool execution from arbitrary terminal output.

## Active delivery scope

- Desktop: **macOS, Windows and Linux are all first-class GPUI targets**, with one shared UI codebase and OS-specific Host transports.
- Mobile: **SwiftUI iOS / iPadOS** stays in active scope for QR pairing, session access, approvals and floating Remote Screen.
- **Android, Web and TUI are deferred.** Do not scaffold their applications or schedule their implementation unless explicitly requested. Keep protocol and domain boundaries extensible.

## Workspace persistence rules

- `ukk-domain` defines persisted Space / Session / AgentRun and Pane identity and tree validation. `ukk-store` owns SQLite schema and optimistic layout revision updates. Do not write SQLite directly from GPUI.
- `AgentRun` is a product-level binding to `core_id + terminal_address`. Creating or archiving metadata never launches or terminates a herdr-core process.
- **Do not auto-assign orphan terminals to arbitrary Sessions.** Bind only with verified core identity and user intent.
- Closing a Pane, switching a Session, archiving a Session, or quitting GPUI must not send any terminal-stop/kill signal.
- Current local Host RPC `workspace.snapshot` is **read-only**. Do not add new mutation methods before local permissions and peer validation are designed.
- Run `cargo test --workspace` including SQLite reopen, CAS conflicts and cross-Session Agent reference tests. See [Workspace Persistence](docs/WORKSPACE_PERSISTENCE.md).

## Working practices

- Verify upstream APIs, versions, licenses and schemas before adding dependencies.
- Call out unknowns, implement testable contracts, keep docs and diagrams updated.
- Avoid writing placeholder implementations that claim features already work.
- Require explicit tests for security controls, device revocation, cursor gaps, and plugin permissions.
- Protect user code and credentials: never commit local keys or tokens.
- Match approved Design Direction v0.1: light native multi-pane desktop, mobile chat-first, floating screen tile.

## Before declaring a feature complete

Describe the user journey; provide reproducible build/run steps; show tests; identify platform constraints; verify offline/reconnect and permission-denial cases where relevant.

See [docs/ROADMAP.md](docs/ROADMAP.md) for milestones. Initial code should build a small vertical slice rather than introducing every module as a stub.
