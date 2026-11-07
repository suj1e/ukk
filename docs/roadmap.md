# ukk — Delivery roadmap and parallel Agent work queue

> Updated 2026-11-07. Live evidence and open gaps: [status.md](status.md). This replaces the stale progress summary in the former uppercase `ROADMAP.md`; the full product vision remains in [PRODUCT.md](PRODUCT.md).
>
> **目标：** 完整的跨设备、多 Agent 原生 Workspace；不是固定流水线，也不是单独的远控产品。Desktop = GPUI on macOS/Windows/Linux. Mobile = SwiftUI iOS/iPadOS. herdr-core owns Agent execution. Remote Screen is a permissioned floating feature *inside a Session*.

## Rules for all Agent contributors

1. **Take a task ID** from this queue, work on a dedicated branch, link an existing issue or create a narrowly scoped follow-up issue as needed, and open one reviewable PR. A PR is not "done" until acceptance evidence is recorded.
2. Read [AGENTS.md](../AGENTS.md), [ENGINEERING.md](ENGINEERING.md), [ARCHITECTURE.md](ARCHITECTURE.md), [DOMAIN_MODEL.md](DOMAIN_MODEL.md), [SECURITY.md](SECURITY.md). Keep domain/runtime/view and local/remote authority boundaries strict.
3. **Never stop herdr-core processes when a Pane/Tab/Session/window is closed.** Do not fabricate structured Agent events from ANSI text. No unapproved remote mutation, screen capture or input.
4. Shared integration hot spots: `crates/host/src/rpc.rs`, `workspace_rpc.rs`, `crates/host-client/src/lib.rs`, `crates/store/src/schema.rs` and `apps/desktop/src/workspace/{mod,actions,state}.rs`. Assign one integration owner; coordinate RPC/schema contracts before concurrent edits.
5. Verify on the **exact PR head**; root CI covers macOS/Windows/Linux Rust tests/Clippy, Desktop CI covers macOS/Windows/Linux native check/tests/Clippy. Actual GUI and real herdr-core acceptance remain separate release gates.
6. Product scope is **not** narrowed because a task is deferred. Android/Web/TUI are deferred, **not** part of active scaffolding or CI. A headless Host is a runtime capability, not a TUI product.

## Priority and dependency map

```text
Q0  main CI + 3-OS real baseline ───────────────┐
D1  PR #12 cross-Pane Tabs ──> D2 keyboard/a11y │
R1  real hc control + attach ──> R2 adapters ──> D3 conversation/diff/terminal
H1  Host lifecycle + IPC hardening ─────────────┤
P1  plugin API/capability boundaries ───────────┤
S1  durable remote protocol ──> S2 QR/relay ───┤─> I1 real iOS session
I1  SwiftUI fixture-first shell ────────────────┤
P1 + S2 + I1 + H1 ──────────────────────────────┴─> M1 floating Remote Screen
Q1  release/security/integration audit spans all milestones
```

The arrows indicate **integration dependencies**, not an excuse to block independent contract tests, mock shells or spikes. `D1` means **review/complete the existing PR #12**, not duplicate its implementation.

## Wave A — Stabilize the existing foundation (P0, immediately parallel)

### Q0 — CI integrity, three-OS UI and hc acceptance

- **Suggested ownership:** QA / CI Agent.
- **Edit first:** `crates/hc-client/tests/mock_protocol.rs`, `.github/workflows/*`, dedicated test fixtures and acceptance reports. Avoid changing production parsing without a failing reproduction.
- **Do:** reproduce [main Ubuntu failure](https://github.com/suj1e/ukk/actions/runs/42385779740) at `17fa080` (`watches_snapshot_event_and_missed` EOF error expectation). Distinguish protocol bug from nondeterministic test scheduling or mock process exit. Add a deterministic regression, run repeated tests on Linux, do not simply weaken the assertion. Record the actual cause.
- **Live matrix:** boot Host + real `hc` on macOS/Windows/Linux where environments are available; open GPUI; create/archive/reopen Session; split/drag/nest panes; reconnect; close window and prove Agent survival. Test dual-window metadata sync and local file permissions. **Where hardware is unavailable, mark NOT VERIFIED** and provide a reproducible runbook.
- **Exit:** root and Desktop cross-platform jobs green at exact commit; mock protocol deterministic; real-device evidence explicitly separated by OS, with screenshots/logs only where actually captured.
- **Current:** source tests exist; main root Linux job failed, Desktop job green. CI on unrelated PR heads does not close this task.

### D1 — Finish cross-Pane Tab move (existing PR #12)

- **Suggested ownership:** Desktop interaction Agent; **take over/review [PR #12](https://github.com/suj1e/ukk/pull/12)** rather than opening a competing feature branch.
- **Existing work:** `crates/domain/src/tab_transfer.rs`, `crates/host/src/workspace_rpc.rs`, GPUI `tabs.rs` / `panes.rs` / `actions.rs`; one authorized `workspace.tab.move` / SQLite CAS mutation.
- **Do:** verify head CI, source review nested drop event propagation, drag cancellation/out-of-bounds, cross-Session rejection, stable IDs/AgentRun retention and conflict UX. If changing active shared modules, serialize merges with `D2`.
- **Exit:** all 6 platform jobs green; regression tests and actual native GUI mouse smoke recorded; no view-triggered Agent termination. Non-blocking future enhancements (insertion index, keyboard move) are documented, not misrepresented as delivered.

### D2 — Keyboard, accessibility and local-only focus lifecycle

- **Suggested ownership:** GPUI UX Agent (start design/test contract now; merge after D1 where files overlap).
- **Edit:** `apps/desktop/src/workspace/{tabs,panes,state,render}.rs`, local settings adapter; use existing Domain CAS only for *shared layout*, never for local focus.
- **Do:** keyboard tab and Pane traversal, shortcuts for add/close/reorder/move/resize, accessible labels and focus visibility, screen reader/tooltip behavior, focus persistence **per device across restart**, layout recovery from changed remote state, small-window bounds and stale CAS feedback. Support light/dark if relevant to acceptance.
- **Exit:** keyboard-first flows and focus reconciling tests, explicit macOS/Windows/Linux real-window accessibility checklist; no cross-device focus theft.

### H1 — Host process lifecycle and local IPC security

- **Suggested ownership:** Systems / OS Agent.
- **Edit:** `crates/host/src/{transport,local_auth,cli}.rs`, `crates/host-client/src/windows_pipe.rs`, service/packaging scripts; request centralized RPC changes via integration owner.
- **Do:** user-scoped persistent Host lifecycle for launchd, Windows user process, systemd --user; startup/restart/upgrade logic independent of Desktop windows. Test Windows SID ACL and different-user denial, peer-identity assumptions, pipe hijack/remote rejection, bounded read/write deadlines and cancellation, per-client concurrency/quotas. Secure Unix socket directory/permissions and safe cleanup. No TCP fallback.
- **Exit:** security tests and denial evidence; offline/reconnect behavior on every desktop OS; clear installation and recovery commands; no Agent kill when Host/Desktop restarts.
- **Dependency:** coordinates with R1 local mutation permissions; do not widen allowlist implicitly.

## Wave B — A genuinely useful local Agent Workspace (P0/P1, start contracts in parallel)

### R1 — herdr-core integration: real lifecycle, input and attach

- **Suggested ownership:** Runtime Agent.
- **Edit:** `crates/hc-client` for pinned API contracts, new focused Host runtime service modules; request owned RPC envelope changes from integrator.
- **Do:** record real `hc schema`, `hc connect` and terminal framed attach protocol. Verify the actual operations supported for launch/attach/send/interrupt/stop and status/child relationship; implement only capability-checked actions the upstream API actually supplies. Persist verified `core_id + terminal_address` for AgentRun bindings; explicit user intent, typed failure and idempotency where relevant. Separate detach/archive from stop/kill.
- **Exit:** two Agents, child spawn, Host/Desktop restart reattachment, stream backpressure, reconnect/gap tests; named upstream commit/version and actual on-device proof. No guessed approvals or runtime state.
- **Dependency:** local authentication and capability design with H1; does **not** require remote device protocol.

### R2 — Structured Agent adapters and conversation contract

- **Suggested ownership:** Provider Adapter Agent.
- **Edit:** new `crates/agent-adapters/`, versioned provider fixtures/contracts. Avoid modifying `crates/host/src/rpc.rs` during initial implementation.
- **Do:** capability-detect Codex/Claude-compatible structured turns, messages, tools, progress, artifacts and approvals **only where the provider gives explicit semantics**. Keep raw terminal attachment as a documented fallback. Handle resumed sessions, duplicate IDs, partial streams, redaction, unsupported action types and approval provenance.
- **Exit:** fixture-driven parse/merge tests; provider/version matrix; one integrated demo wired through R1 with truthful unsupported states.
- **Dependency:** R1 API/provenance for live integration; fixture-based development independent.

### D3 — Real Session panes: chat, terminal, diff/files/activity

- **Suggested ownership:** Native Product UI Agent.
- **Edit:** new focused modules beneath `apps/desktop/src/workspace/` and new read-only Git/worktree service boundaries. Coordinate `actions.rs` edits.
- **Do:** render real per-Agent conversation/events and framed terminal attachment; read-only Git status, files, diff/hunks and activity; session navigation, source/error/loading indicators, copyable diagnostics and safe no-data state. Do not synthesize fake chats. Add reviewed write operations only behind specific capabilities. Preserve independent Pane/Tab membership and Agent lifetimes.
- **Exit:** start two Agents, switch content views, inspect real changed files, close/reopen UI and retain ongoing runtime; proof via native window and tests on all three platforms.
- **Dependency:** R1/R2 structured data; read-only file/diff views can start against explicit fixtures now.

## Wave C — Extensibility and secure cross-device foundations (P1, independent contracts)

### P1 — First-party plugin API and capability broker

- **Suggested ownership:** Plugin Platform Agent.
- **Edit:** new `crates/plugin-api/`, `crates/plugin-host/`, sample plugin and [PLUGINS.md](PLUGINS.md). Keep existing GPUI/Host integration to a thin, reviewed adapter.
- **Do:** versioned manifest, declared commands/events, narrowly scoped native UI slots (including Session tabs/overlays), user grants, lifecycle, enable/disable, isolated helper supervision, crash containment, audit. Distinguish trusted in-process contributions from untrusted helpers.
- **Exit:** harmless sample Session action + tab/overlay contribution enabled/disabled without Host restart; permission-denial, forged command and crash tests. No privileged capture plugin before the broker exists.
- **Dependency:** coordinate future M1 screen plugin and R2 provider adapter surfaces.

### S1 — Remote Host protocol, durable event sync and authorization

- **Suggested ownership:** Protocol/Security Agent.
- **Edit:** new `crates/protocol/`, Host service adapters, focused SQLite event/migration modules, [PROTOCOL.md](PROTOCOL.md)/[SECURITY.md](SECURITY.md).
- **Do:** versioned typed command/event envelopes, server-side scopes, device identity/grants, monotonic per-stream cursor, snapshot/replay on gaps, idempotency keys and result classification. **Never** use ephemeral `hc_seq` or Host `host_epoch` as a durable cross-device cursor. Bound payload sizes, queues and retention; forbid blind replay of approvals or pointer input.
- **Exit:** mock two-client reconnect/revocation/duplication/dropped-ACK/out-of-order/gap tests; security review and migration strategy; local IPC remains distinct from WAN protocol.
- **Dependency:** H1 authorization boundary; can build protocol crate and contract tests in parallel with R1/R2.

### S2 — QR pairing, device trust, Relay integration

- **Suggested ownership:** Pairing/Transport Agent.
- **Edit:** new `crates/remote/`, pairing/transport adapters, [PAIRING.md](PAIRING.md).
- **Do:** short-lived QR challenge, desktop confirmation, device key storage, scoped grants, expiry/rotation/revoke and unauthenticated denial. Spike against **actual** alma-relay behavior before reuse; end-to-end encryption and device trust independent of transport. Outbound Host connection; no unapproved public inbound port.
- **Exit:** connect iPhone test client outside LAN, resume exact Session, revoke device and prove command denial, rotate keys, handle offline/relay outage; repeatable packet/contract tests and a documented TURN/media signalling boundary.
- **Dependency:** S1 wire/auth contract. Pairing crypto and relay compatibility spikes can be separate PRs.

### I1 — Native SwiftUI iOS/iPadOS companion (fixture-first)

- **Suggested ownership:** iOS Agent.
- **Edit:** new `apps/ios/`; do **not** introduce WebView, Android or a GPUI iOS fork.
- **Do:** build Workspace/Space/Session/Agent navigation, chat-first Session screen, live/offline indicators, approvals only where explicitly supported, trusted-device UI and QR scan presentation. Use deterministic mock data clearly labelled as fixtures until S1/S2 work. Add reconnect, backgrounding and notification interface contracts; APNs delivery is not proven until entitlement/device testing.
- **Exit:** simulator build + UI/unit tests; on-device QA separately; then a paired physical iPhone reconnects to real Host sessions and can send an authorized Agent command.
- **Dependency:** implement shell in parallel, integrate S1/S2/R1 afterward. No floating screen yet except a reserved extension slot.

## Wave D — Session-scoped Remote Screen (P2; not a separate product)

### M1 — Floating Remote Screen plugin

- **Suggested ownership:** Media + Native UX Agents split into two PRs with an agreed interface.
- **Edit:** `plugins/remote-screen/` and OS capture/input helpers; iOS Session overlay contribution. Keep media outside event history/relay payloads.
- **Do:** explicit `view` grant then separate `control` grant; macOS ScreenCaptureKit/VideoToolbox, Windows Graphics Capture and Linux PipeWire/portal integration with honest unsupported cases. Encrypted low-latency WebRTC/media path with STUN/TURN validation; relay is signalling/control-plane only. Show a floating compact preview over Agent chat, expand for pointer/keyboard control, minimize back to the **same Session**. Clearly indicate capture/control, expiring controller lease, device lock pause and immediate stop/revocation.
- **Exit:** real iPhone and each supported desktop capture adapter demonstrated; permission denied, strict NAT, sleep, disconnect, another controller and stop/revoke tested; no streaming/capture after stop, no permanent screen navigation mode.
- **Dependencies:** P1 plugin broker, S2 trust, I1 Session UI and H1 secure Host lifecycle. Begin independent codec/capture spikes now but **do not** expose control before the gates.

## Wave E — Release engineering and completion (cross-cutting)

### Q1 — Packaging, migrations, observability and security release gate

- **Suggested ownership:** Integration/Release Agent.
- **Do:** installer/updater and user-service setup on macOS, Windows, Linux; native application signing where applicable; SQLite migration/backup/recovery; logs and diagnostics without credentials; perf/memory and multi-observer backpressure tests; accessibility and locale; threat review, plugin sandbox and device revocation checks. iOS TestFlight/App Store requirements separately.
- **Exit:** documented end-to-end matrix showing *actual* running Agents survive Desktop close, reattach on all three OSes, an iPhone pairs over a different network, an Agent instruction and approval work where supported, and Remote Screen opens/closes on demand without leaving the Session. Every unsupported combination named.
- **Dependency:** continuous quality owner throughout all waves, final closure after D3 + P1 + S2 + I1 + M1.

## Parallelization / merge discipline

| Lane / task(s) | Can start now? | Primary files | Merge sequencing / conflict warning |
| --- | --- | --- | --- |
| QA: `Q0` | Yes | mock tests, CI fixtures, evidence | Fix `main` regression first; don't mask failures |
| Desktop drag: `D1` | Yes — **existing PR #12** | Domain tab transfer + GPUI tabs/panes | Don't implement a duplicate; integrate before `D2` |
| UX: `D2` | Yes, designs/tests first | GPUI tabs/state | Serialize GPUI edits with `D1` |
| Runtime: `R1` | Yes | hc-client / new Host runtime modules | Coordinate auth/RPC with `H1` and `S1` |
| OS/IPC: `H1` | Yes | transport, Windows pipe, service scripts | No independent authorization bypass or simultaneous `rpc.rs` edits |
| Adapters: `R2` | Yes, fixtures/contracts | new agent-adapters crate | Integrate only after R1 live API is verified |
| UI content: `D3` | Yes, read-only/fixture slice | new GPUI view modules, git read adapters | Coordinate actions/state integration with D1/D2/R1 |
| Plugins: `P1` | Yes | new plugin crates | Agree capability schema; keep initial diff self-contained |
| Protocol: `S1` | Yes | new protocol crate | Plan store migrations and RPC handshake before integration |
| Pairing: `S2` | Compatibility spike now | new remote crate | Full feature gated on S1 and H1 |
| iOS: `I1` | Yes, clearly fixture-based | new apps/ios | Real sessions require S1/S2/R1 |
| Screen: `M1` | Capture spike only | plugin/OS media adapters | Actual control gated on P1/S2/I1/H1 |
| Release: `Q1` | Continuous | manifests, tests, docs | Final acceptance only after integration |

**First useful assignment batch:** `Q0`, `D1`, `R1`, `H1`, `P1`, and `I1` in separate worktrees; `R2` and `S1` are good additional parallel contract lanes. One human/integration Agent arbitrates shared RPC/database changes.

## Exact completion checklist for each task

- [ ] Linked issue/task ID, explicit user journey, scoped changes and dependency review.
- [ ] Unit/contract tests cover success, failure, cancellation, stale revision, restart and permission-denial paths relevant to that task.
- [ ] Root `cargo fmt --all -- --check`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` if root crates changed.
- [ ] Desktop `cargo fmt --manifest-path apps/desktop/Cargo.toml --all -- --check`, `cargo check --manifest-path apps/desktop/Cargo.toml`, `cargo test --manifest-path apps/desktop/Cargo.toml --bin ukk-desktop`, `cargo clippy --manifest-path apps/desktop/Cargo.toml --all-targets -- -D warnings` if Desktop changed.
- [ ] macOS/Windows/Linux relevant CI checks linked with exact commit SHA. Docs-only PRs may not trigger path-filtered workflows; state that explicitly rather than claiming CI pass.
- [ ] Actual native/physical-device evidence where the task claims real behavior; otherwise **NOT VERIFIED** and reproducible runbook.
- [ ] No Pane/Tab/Session/window closure causes `terminal.stop`/`kill`; no hidden escalation of Agent/plugin/remote/screen permissions.
- [ ] Compatibility/schema/migration/security docs updated; PR merged only after source review and appropriate gates.
- [ ] Integration owner updates [status.md](status.md), task progress and open acceptance separately after merge.

## Milestones and product acceptance

- **M0 — Verified baseline:** Q0 + existing PR #12 disposition, demonstrably stable three-OS CI, at least one real GUI smoke and real hc integration path.
- **M1 — Useful native Desktop:** D2 + R1 + R2 + D3 + H1. Parallel Agents can actually receive instructions, stream results, and persist across window/Host restarts. Native pane UX must pass release QA.
- **M2 — Extensible Host:** P1 + S1. A sample plugin works with explicit permission denial; replay/authorization contract passes independent of iOS.
- **M3 — Remote companion:** S2 + I1. Paired iPhone over WAN resumes exact running Session and can perform authorized operations, with revoke/offline behavior.
- **M4 — On-demand floating screen:** M1. Screen preview/control within Session only, explicit grants, secure stop and NAT/permissions matrix.
- **M5 — Ship-worthy integration:** Q1 across all supported OSes, device security, migrations, observability and documented release artifacts.

No calendar deadlines are promised. Completion is measured by observable acceptance, not by generated source or checkboxes alone.
