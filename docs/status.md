# ukk — Implementation status

> **Snapshot:** 2026-11-08, approximately 01:35 UTC. Baseline is `main` at `ef9038c` (Q0, H1, D2, R2 and P1 merged; iOS PR #21 open). This is a source/CI inventory, **not** a claim of on-device acceptance. Refresh this document after material PRs land.
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
| herdr-core adapter | `crates/hc-client` JSON-RPC bridge; terminal list and `events.subscribe` parsing | Mock transport tests deterministic on three OSes; real `hc 0.1.18` `doctor`/`list`/`watch` verified on macOS and in three-OS CI. Launch/attach/send/interrupt lifecycles remain outstanding (R1) |
| Host lifecycle / IPC hardening | `crates/host/src/{lifecycle,local_fs,peer,transport}.rs`, launchd/systemd/Scheduled Task templates, overlapped Windows pipe I/O, verified stale-socket recovery — [PR #16](https://github.com/suj1e/ukk/pull/16) | Root, Desktop and real-hc CI green on macOS/Ubuntu/Windows; macOS SIGTERM→stale→restart recovery verified with a live hc terminal. Real service install/uninstall and different-user denial remain **NOT VERIFIED** (runbook in `docs/HOST_LIFECYCLE.md`) |
| Desktop keyboard / focus | `keymap.rs`, `navigation.rs`, `pane_focus.rs`, `focus_store.rs`; local per-device focus, CAS conflict UX, a11y roles — [PR #17](https://github.com/suj1e/ukk/pull/17) | Desktop CI green on three OSes with 39 unit tests; real window/screen-reader acceptance **NOT VERIFIED** |
| Structured Agent adapters | New `crates/agent-adapters` v1 contract, Codex app-server v2 and Claude stream-json fixtures, merge/replay/redaction — [PR #18](https://github.com/suj1e/ukk/pull/18) | Fixture and merge tests green on three OSes; no live provider run or R1 wiring |
| Host event broker | `crates/host/src/broker.rs`; reconnect, missed/gap recovery; `host.snapshot`/`host.wait`; read-only observation | Source and integration tests; no durable remote event replay |
| Desktop Host transport | Shared local client; Unix socket on macOS/Linux and SID-scoped Windows Named Pipe | Code and cross-platform CI; H1 hardening adds peer identity, permissions, deadlines and quotas. No real-user Windows ACL/different-user acceptance |
| Durable metadata | `ukk-domain` + `ukk-store` SQLite Spaces/Sessions/AgentRuns, pane layouts, CAS and migrations | SQLite unit/integration coverage; no three-OS real GUI persistence acceptance |
| Cross-window metadata | SQLite v2 DB epoch/revisions, `workspace.state`/`workspace.wait`, GPUI reconciliation — [PR #8](https://github.com/suj1e/ukk/pull/8) | CI-verified on PR; no recorded dual real-window smoke |
| Native layout | Split/close/resize, horizontal/vertical drag with single CAS commit — [PR #9](https://github.com/suj1e/ukk/pull/9) | PR CI green; real mouse/nested layout acceptance pending |
| Native tabs | `PaneNode::Tabs`, persistent membership/order, per-window focus, add/close/reorder — [PR #11](https://github.com/suj1e/ukk/pull/11); cross-Pane move with one CAS — [PR #12](https://github.com/suj1e/ukk/pull/12) | PR CI green; real mouse and keyboard/accessibility acceptance deferred to Q0/D2 |
| Deterministic hc test transport | Compiled `mock_hc` fixture via `CARGO_BIN_EXE_mock_hc`; no runtime script creation — [PR #14](https://github.com/suj1e/ukk/pull/14) | Root CI green on macOS/Ubuntu/Windows; eliminates the ETXTBSY flake |
| Plugin platform | `crates/plugin-api` schema v1, `crates/plugin-host` supervisor/grant/dispatch/audit, sample + fault helpers — [PR #20](https://github.com/suj1e/ukk/pull/20) | Three-OS tests and real-hc acceptance green; security review blocker (timed-out helper containment) fixed. Headless broker only — no Host RPC/GPUI slot wiring, no OS sandbox |
| GPUI native launch smoke | `.github/workflows/desktop-launch-smoke.yml` + bounded X11/CoreGraphics harness — [PR #22](https://github.com/suj1e/ukk/pull/22) | macOS and Linux CI prove a real mapped/owned window and Host survival; Windows interactive desktop NOT VERIFIED |
| Code organization | Small entrypoints; Desktop `workspace/*`, Host transport/RPC/broker, Domain and SQLite modules — [PR #7](https://github.com/suj1e/ukk/pull/7) | Three-OS CI for that PR; process and UI runtime acceptance pending |

The Host has a **narrow local capability-protected mutation allowlist** for Session creation and Pane/Tab layout edits. This is *not* general Agent command authorization, remote device trust, or a complete identity/security review. Some older docs/issues describe these implemented pieces as pending; this status supersedes outdated progress claims, not their remaining acceptance requirements.

## In-flight / observed CI

- **Q0 (merged):** [PR #14](https://github.com/suj1e/ukk/pull/14) fixed the Ubuntu `ETXTBSY` test-fixture race; [PR #15](https://github.com/suj1e/ukk/pull/15) added a pinned, checksum-verified hc 0.1.18 acceptance job that passed on **macOS, Ubuntu and Windows CI** (two live terminals, `doctor`/`list`, `watch` snapshot + `created`, survival after the Host viewer stops).
- **H1 (merged as `e3b112b`, [PR #16](https://github.com/suj1e/ukk/pull/16)):** user-scoped lifecycle templates/commands, 0700/0600 Unix socket + same-user peer checks, verified stale-socket recovery, Windows overlapped pipe I/O with deadlines and SID checks, quotas. An independent security review found a Windows token-rotation ordering regression and recovery gaps; fixed before merge. On main + real hc 0.1.18, SIGTERM → stale socket → restart → `terminal.list` works and the terminal stays live.
- **D2 (merged as `8317677`, [PR #17](https://github.com/suj1e/ukk/pull/17)):** keyboard navigation, per-device focus persistence, CAS conflict UX, accessibility roles. Independent review found a startup focus-wipe blocker and five majors; all fixed with regression tests. Desktop CI green on three OSes (39 tests).
- **R2 (merged as `5410c9d`, [PR #18](https://github.com/suj1e/ukk/pull/18)):** versioned structured adapter contract, merge/replay/redaction, Codex/Claude fixtures. Independent review found four streaming/merge blockers; fixed with fixtures/tests. Three-OS CI green.
- **Open:** [PR #20 plugins](https://github.com/suj1e/ukk/pull/20) (security-review blocker fixed at head `5f5aecb`, CI finishing); [PR #21 iOS shell](https://github.com/suj1e/ukk/pull/21) (iOS build + 58 unit tests green in CI; secret-echo/state fixes in progress).
- **Q0 launch smoke (merged as `b974fca`, [PR #22](https://github.com/suj1e/ukk/pull/22)):** macOS `CGWindowList` and Linux X11/`xdotool` jobs prove that the native Desktop owns a real mapped layer-0 window and that terminating it leaves `ukk-host serve` alive. This is the first automated live-window evidence; real mouse/keyboard/a11y and a Windows interactive desktop remain **NOT VERIFIED**.
- **Still NOT VERIFIED:** real native-window interaction/accessibility on each OS, real launchd/systemd/Scheduled Task installation, different-user denial, live Codex/Claude provider runs, and real-hc launch/attach/send/interrupt (R1).
- CI builds and tests **do not by themselves demonstrate live mouse, screen-reader, service-install or iPhone use**; the new launch-smoke workflow is the first automated window-creation check.

## Open product gaps

| Priority | Gap / current boundary | Task IDs in roadmap |
| --- | --- | --- |
| P0 | Automated launch smoke landed (#22, macOS/Linux); real mouse/keyboard/screen-reader and Windows interactive-desktop acceptance remain | `Q0` |
| P0 | Keyboard/a11y/focus landed (#17); real VoiceOver/Narrator/Orca and physical restart acceptance remain | `D2` |
| P0 | Verify real `hc` APIs and attach stream; user-authorized launch/send/interrupt with explicit provenance and lifetimes | `R1` |
| P0 | Structured adapter contract landed (#18); wire live providers through R1 and build real conversation/terminal/diff surfaces | `R2`, `D3` |
| P0 | Host lifecycle/IPC landed (#16); real launchd/systemd/Scheduled Task install and different-user denial remain | `H1` |
| P1 | Plugin API/broker merged (#20); Host RPC/GPUI slot wiring, durable grants/audit and OS sandbox remain | `P1` |
| P1 | Durable auth-aware Host/device protocol, event replay/resume, idempotency and version negotiation | `S1` |
| P1 | QR device trust/revocation and relay compatibility/prototype; SwiftUI shell in [PR #21](https://github.com/suj1e/ukk/pull/21) (fixtures only) | `S2`, `I1` |
| P2 | Opt-in floating Remote Screen: view/control separation, OS capture adapters, low-latency media + TURN | `M1` |
| Release | Security, packaging, migration/backups, E2E, docs and release readiness across active platforms | `Q1` |

`crates/plugin-api` and `crates/plugin-host` are merged as source (headless broker tests only); no Host RPC or GPUI slot wiring exists yet. On `main` there is still **no** `apps/ios`, `crates/protocol` or Remote Screen plugin implementation. Open PR #21 is the first `apps/ios` implementation; do not treat it as shipped until merged. `crates/agent-adapters` is merged and fixture-verified only.

## Existing issue/PR pointers

- [#1](https://github.com/suj1e/ukk/issues/1) real Mac hc/Host acceptance; [#2](https://github.com/suj1e/ukk/issues/2) GPUI baseline; [#3](https://github.com/suj1e/ukk/issues/3) event broker; [#4](https://github.com/suj1e/ukk/issues/4) cross-platform Host/IPC; [#5](https://github.com/suj1e/ukk/issues/5) SQLite/product persistence; [#6](https://github.com/suj1e/ukk/issues/6) interactive Desktop layout and native acceptance.
- [#10](https://github.com/suj1e/ukk/issues/10) was **closed** after PR #11; cross-Pane move landed in #12 and keyboard/focus in #17. Remaining real-window/screen-reader acceptance is tracked in `docs/GPUI_KEYBOARD_A11Y.md`, not by the closed issue.
- Older issues and some historical runbooks contain pre-merge checklist states; verify commit, CI and native evidence instead of copying them forward.
- Open follow-ups created by this wave: [#19](https://github.com/suj1e/ukk/issues/19) plugin platform (P1).

## Agent handoff / status maintenance

1. **Claim one roadmap task ID**. Open a focused topic branch and PR against `main`; link the task ID and the relevant existing issue. Check merged PRs #14–#18 before re-implementing Q0/D1/D2/H1/R2.
2. Preserve [AGENTS.md](../AGENTS.md) invariants. Avoid overlapping edits to `crates/host/src/rpc.rs`, `workspace_rpc.rs`, `crates/store`, or `apps/desktop/src/workspace` without coordinating integration order. New packages/contracts can be built in parallel.
3. PR description must record: *implemented*, *CI on exact SHA*, *manual hardware/runtime proof*, *unverified*, *security consequences*, *dependent PRs*. No silent "done" based on compilation.
4. Source-only tasks must pass the root Rustfmt/tests/strict Clippy gate when affected. Desktop changes must pass native format, check, tests and strict Clippy on macOS/Windows/Linux. Real OS UI/system permissions require separate explicit acceptance evidence.
5. **One integration owner** updates this status and roadmap after merges to avoid multiple agents overwriting progress. Mark `source / CI / native` independently. Update platform matrix and design docs in the feature PR when appropriate.

**Next wave:** `R1` (hc launch/attach/input), `D3` (read-only surfaces), `S1` (protocol contract), `I1` (#21 fixes then S1/S2 integration), `P1` Host/GPUI wiring, followed by `S2` and `M1`. See [roadmap](roadmap.md) for precise ownership and dependencies.
