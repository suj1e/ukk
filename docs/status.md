# ukk — Implementation status

> **Snapshot:** 2026-11-07, approximately 15:37 UTC. Baseline is `main` at `17fa080d97f9ba59634f1eaca1a0a92250b354c1`. This is a source/CI inventory, **not** a claim of on-device acceptance. Refresh this document after material PRs land.
>
> **中文速览：** GPUI 多窗口/持久化布局已有基础；Agent 实际交互、真机验收、插件、远程配对、SwiftUI iOS 和会话内悬浮远控尚未完成。见 [可认领任务与依赖](roadmap.md)。

## Product contract (do not redesign)

- **Route B:** new Rust/GPUI Desktop on **macOS, Windows and Linux**, not a fork of herdr-app.
- **Runtime:** herdr-core owns persistent terminals/Agent processes. The ukk Host owns product-level session metadata, authorization and synchronization; GPUI panes and windows **never** own Agent lifetime.
- **Domain:** Space → Session → AgentRun; Pane / Tab is a view and may render any authorized content. No mandatory Planner/Coder/Reviewer/Tester workflow.
- **Mobile:** native SwiftUI iOS/iPadOS remains active scope. Android, Web UI and TUI remain explicitly deferred.
- **Remote Screen:** opt-in session plugin, initially a floating preview above the chat that expands temporarily for control. It is never a standalone mode. Viewing and controlling require separate grants.
- **Safety:** never guess structured approvals or turns from terminal text; no remote mutation, capture or input without explicit authentication, authorization and consent.

Canonical constraints: [AGENTS.md](../AGENTS.md), [Product](PRODUCT.md), [Architecture](ARCHITECTURE.md), [Security](SECURITY.md).

## Landed on main (code inventory)

| Area | Source on main | Validation boundary |
| --- | --- | --- |
| herdr-core adapter | `crates/hc-client` JSON-RPC bridge; terminal list and `events.subscribe` parsing | Mock tests exist; **real herdr-core compatibility and lifecycle acceptance remain outstanding** |
| Host event broker | `crates/host/src/broker.rs`; reconnect, missed/gap recovery; `host.snapshot`/`host.wait`; read-only observation | Source and integration tests; no durable remote event replay |
| Desktop Host transport | Shared local client; Unix socket on macOS/Linux and SID-scoped Windows Named Pipe | Code and cross-platform CI, but no completed real-user Windows ACL/blocked-I/O acceptance |
| Durable metadata | `ukk-domain` + `ukk-store` SQLite Spaces/Sessions/AgentRuns, pane layouts, CAS and migrations | SQLite unit/integration coverage; no three-OS real GUI persistence acceptance |
| Cross-window metadata | SQLite v2 DB epoch/revisions, `workspace.state`/`workspace.wait`, GPUI reconciliation — [PR #8](https://github.com/suj1e/ukk/pull/8) | CI-verified on PR; no recorded dual real-window smoke |
| Native layout | Split/close/resize, horizontal/vertical drag with single CAS commit — [PR #9](https://github.com/suj1e/ukk/pull/9) | PR CI green; real mouse/nested layout acceptance pending |
| Native tabs | `PaneNode::Tabs`, persistent membership/order, per-window focus, add/close/reorder — [PR #11](https://github.com/suj1e/ukk/pull/11) | PR CI green; cross-Pane drag and accessibility were deferred |
| Code organization | Small entrypoints; Desktop `workspace/*`, Host transport/RPC/broker, Domain and SQLite modules — [PR #7](https://github.com/suj1e/ukk/pull/7) | Three-OS CI for that PR; process and UI runtime acceptance pending |

The Host has a **narrow local capability-protected mutation allowlist** for Session creation and Pane/Tab layout edits. This is *not* general Agent command authorization, remote device trust, or a complete identity/security review. Some older docs/issues describe these implemented pieces as pending; this status supersedes outdated progress claims, not their remaining acceptance requirements.

## In-flight / observed CI

- **[PR #12 — cross-Pane Tab move](https://github.com/suj1e/ukk/pull/12)**: **open Draft**, head `c5be43b798aca4b489e377c512fbe030fd8ee782` at observation. Implements `PaneNode::move_tab`, local capability-protected `workspace.tab.move`, one SQLite CAS and GPUI Leaf/Tabs drop targets. **Not merged**. Real mouse, nested-target, stale-revision, keyboard and accessibility acceptance remain open. See the PR branch's [TAB_TRANSFER.md](https://github.com/suj1e/ukk/blob/feat/cross-pane-tab-move/docs/TAB_TRANSFER.md).
- **PR #12 core/Host CI:** 3/3 macOS/Ubuntu/Windows passed for [run 42387121345](https://github.com/suj1e/ukk/actions/runs/42387121345). Its Desktop [run 42387121401](https://github.com/suj1e/ukk/actions/runs/42387121401) was **in progress** when checked; recheck before any merge.
- **Latest observed main CI** for `17fa080`: [Desktop run 42385779703](https://github.com/suj1e/ukk/actions/runs/42385779703) succeeded on all three OSes; [root run 42385779740](https://github.com/suj1e/ukk/actions/runs/42385779740) **failed on Ubuntu**, while macOS/Windows passed. Failure: `crates/hc-client/tests/mock_protocol.rs::watches_snapshot_event_and_missed` expected `HcError::Protocol` on mock EOF. Cause is **unverified**; do not label fixed merely because a later PR branch passed.
- CI builds and tests **do not open native windows**. None of these checkmarks demonstrate live drag, accessibility, launch-agent workflows, Windows different-user access denial or iPhone use.

## Open product gaps

| Priority | Gap / current boundary | Task IDs in roadmap |
| --- | --- | --- |
| P0 | Stabilize `main` CI, reproduce Linux mock EOF behavior; real macOS/Windows/Linux GPUI, Host and SQLite smoke evidence | `Q0` |
| P0 | Review/finish Draft #12; add tab keyboard navigation, accessible focus, restart-local focus persistence and correct CAS error UX | `D1`, `D2` |
| P0 | Verify real `hc` APIs and attach stream; user-authorized launch/send/interrupt with explicit provenance and lifetimes | `R1` |
| P0 | Actual conversation/terminal/structured turn content, git diff/files/activity with truthful unsupported-state UX | `R2`, `D3` |
| P0 | Host startup/restart/user-service lifecycle; Windows named-pipe security/deadline and Linux Wayland/X11 validation | `H1` |
| P1 | Versioned plugin manifest, capability broker, native session UI slots, enable/disable and crash containment | `P1` |
| P1 | Durable auth-aware Host/device protocol, event replay/resume, idempotency and version negotiation | `S1` |
| P1 | QR device trust/revocation and relay compatibility/prototype; native SwiftUI iPhone/iPad companion | `S2`, `I1` |
| P2 | Opt-in floating Remote Screen: view/control separation, OS capture adapters, low-latency media + TURN | `M1` |
| Release | Security, packaging, migration/backups, E2E, docs and release readiness across active platforms | `Q1` |

There is **no** `apps/ios`, `crates/plugin-api`, `crates/plugin-host`, `crates/protocol` or Remote Screen plugin implementation on the observed `main` tree. Architecture docs describing these are *proposals*, not shipped features.

## Existing issue/PR pointers

- [#1](https://github.com/suj1e/ukk/issues/1) real Mac hc/Host acceptance; [#2](https://github.com/suj1e/ukk/issues/2) GPUI baseline; [#3](https://github.com/suj1e/ukk/issues/3) event broker; [#4](https://github.com/suj1e/ukk/issues/4) cross-platform Host/IPC; [#5](https://github.com/suj1e/ukk/issues/5) SQLite/product persistence; [#6](https://github.com/suj1e/ukk/issues/6) interactive Desktop layout and native acceptance.
- [#10](https://github.com/suj1e/ukk/issues/10) was **closed** after PR #11 but its description still lists keyboard/focus/drag/live-UI gaps; track those in the roadmap and PR #12 rather than treating closure as release evidence.
- Older issues and some historical runbooks contain pre-merge checklist states; verify commit, CI and native evidence instead of copying them forward.

## Agent handoff / status maintenance

1. **Claim one roadmap task ID**. Open a focused topic branch and PR against `main`; link the task ID and the relevant existing issue. Do **not** create a second implementation of already-in-flight PR #12.
2. Preserve [AGENTS.md](../AGENTS.md) invariants. Avoid overlapping edits to `crates/host/src/rpc.rs`, `workspace_rpc.rs`, `crates/store`, or `apps/desktop/src/workspace` without coordinating integration order. New packages/contracts can be built in parallel.
3. PR description must record: *implemented*, *CI on exact SHA*, *manual hardware/runtime proof*, *unverified*, *security consequences*, *dependent PRs*. No silent "done" based on compilation.
4. Source-only tasks must pass the root Rustfmt/tests/strict Clippy gate when affected. Desktop changes must pass native format, check, tests and strict Clippy on macOS/Windows/Linux. Real OS UI/system permissions require separate explicit acceptance evidence.
5. **One integration owner** updates this status and roadmap after merges to avoid multiple agents overwriting progress. Mark `source / CI / native` independently. Update platform matrix and design docs in the feature PR when appropriate.

**Suggested first wave:** `Q0` (CI + live baseline), `D1` (existing PR #12), `R1` (hc runtime contracts), `H1` (Host service/security review), `P1` (isolated plugin contracts), `I1` (fixture-first native iOS shell). See [roadmap](roadmap.md) for precise ownership and dependencies.
