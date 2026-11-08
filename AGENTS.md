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

## Agent process ownership (omp RPC)

- The **Host owns every omp RPC child**. The Desktop, iOS client and plugin helpers never spawn, signal, wait for or reparent an omp process.
- Closing a Pane/Tab/window, switching Sessions or dropping a local client connection must never stop, kill or detach an omp child. Only an explicit `omp.abort` (which cancels the active turn, not the process) or Host shutdown terminates work.
- `omp.start`, `omp.prompt` and `omp.abort` require the separate, default-off **agent-control** capability with `request_id` and `intent: "user"`. `omp.snapshot` and `omp.wait` are read-only and require no capability.
- Never synthesize omp messages, turns, tool calls or approvals, and never infer them from terminal text. Render only typed RPC frames; unsupported/malformed frames are reported truthfully.
- Host restart does not reattach to a running omp session in the current slice. Do not fake resume; start a new session.
- Keep the Host omp timeline bounded (400 entries, 8 KiB per entry, 512 KiB total) and report truncation instead of growing memory.
- See [docs/OMP_RPC.md](docs/OMP_RPC.md) for the wire contract, error codes and NOT VERIFIED items.

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
- The local versioned protocol stream (`~/.ukk/run/agent-host.protocol`) is also **read-only** in the current slice. It carries handshake, snapshot/replay/live events and ACKs only. Do not expose `session_create` or any other mutation there without a protocol-level local capability; workspace mutations remain on the rotated-capability JSON-RPC path.
- Run `cargo test --workspace` including SQLite reopen, CAS conflicts and cross-Session Agent reference tests. See [Workspace Persistence](docs/WORKSPACE_PERSISTENCE.md).

## Code quality is part of feature completion

- Read [Engineering Standards](docs/ENGINEERING.md). Use a topic branch and PR with actual CI evidence rather than committing unverified feature stubs directly to `main`.
- Keep `main.rs` entrypoints small; put GPUI state, actions, panes, navigation and runtime inspector in focused modules. Keep SQLite migrations, records, layouts and snapshots separate.
- Run Rustfmt, tests and Clippy in the root workspace; format and `cargo check` the independent GPUI workspace on macOS/Windows/Linux.
- Review any module growing beyond ~300 lines for multiple responsibilities; refactor by ownership, not arbitrary file size. Preserve API contracts and process-lifetime guarantees.
- Distinguish **source committed** from **CI passing** and **real device verified**. Never claim the latter without matching evidence.

## Working practices

- Verify upstream APIs, versions, licenses and schemas before adding dependencies.
- Call out unknowns, implement testable contracts, keep docs and diagrams updated.
- Avoid writing placeholder implementations that claim features already work.
- Require explicit tests for security controls, device revocation, cursor gaps, and plugin permissions.
- Protect user code and credentials: never commit local keys or tokens.
- Match approved Design Direction v0.1: light native multi-pane desktop, mobile chat-first, floating screen tile.

## Before declaring a feature complete

Describe the user journey; provide reproducible build/run steps; show tests; identify platform constraints; verify offline/reconnect and permission-denial cases where relevant.

See [docs/status.md](docs/status.md) for verified implementation status and [docs/roadmap.md](docs/roadmap.md) for claimable Agent task IDs and milestones. Initial code should build a small vertical slice rather than introducing every module as a stub.
