# Roadmap — full vision, incremental execution

> Dates are intentionally not promised. The product scope is **complete multi-agent workspace + plugin platform + mobile companion**; milestones define risk-reducing delivery order, not permanent omissions.

## Milestone 0 — Source and interface feasibility

- Pin reference commits/versions for herdr-core, GPUI, Alma Relay and chosen media stack.
- Test `hc schema`, JSON-RPC connection, `terminal.attach` framed stream, `events.subscribe`; record exact schemas and compatibility notes.
- Validate how Codex/Claude Code produce structured turns and approval events; define a raw-terminal fallback where no API exists.
- GPUI spike: Spaces/sidebar, one agent chat panel, tab+split layout, keyboard controls.
- Test keeping herdr-core alive after the UI exits and Host/HC restarts.
- Review dependencies/licenses and confirm macOS packaging/permissions.

**Gate:** repeatable local demo, API compatibility report and one-page architecture decision.

## Milestone 1 — Native Desktop & multi-agent foundation

- macOS GPUI UI: Spaces, Session list, Agent tree, tab + panes, command palette, dark/light themes.
- Rust Host as a separate user-scoped background service with SQLite.
- hc adapter: terminal lifecycle, attach, events, machine list and status provenance.
- Launch and control several Agents; parent/child linkage and correct statuses; multi-observer UI.
- Compose first-party structured Agent Adapter; fallback to terminal when provider does not offer structure.
- File diff, worktrees and activity; explicit `close view` versus `stop run`.
- Local logs, diagnostics, restart recovery and update strategy.

**Gate:** two agents run in parallel while desktop window closes and reopens with accurate state.

## Milestone 2 — Plugin system foundation

- Versioned manifest + extension schema, capability broker, first-party plugin lifecycle.
- Plugin commands/actions, subscription filtering, native UI slots and safe enable/disable.
- Runtime split: trusted integrated components versus isolated external helpers.
- Developer SDK/spec examples; permission UX, logging and crash isolation.
- Add first-party integrations through adapters, not hardcoded UI tabs.

**Gate:** a harmless sample plugin renders a session action and can be enabled/disabled without restarting Host; privilege check tests pass.

## Milestone 3 — Secure remote pairing & mobile client

- First-use QR, device identity, desktop approval, expiry, revocation and scopes.
- Adapt or fork Alma Relay after protocol compatibility spike; private control-plane E2EE.
- SwiftUI workspace/agent/session screens, history, send message and approvals where supported.
- Event snapshot+resume, replay cursor, command idempotency, multi-device observation.
- APNs wake-up hints and reconnect UX; offline state, phone lock behavior.

**Gate:** iPhone outside local network resumes the exact running sessions without a permanent UI connection.

## Milestone 4 — Remote Screen as session overlay

- Plugin manifest and permission gates for view and control.
- Mac ScreenCaptureKit/VideoToolbox helper; WebRTC transport with TURN fallback.
- iOS floating preview, tap-to-expand, virtual pointer/keyboard, minimize and close.
- Local capture indicator, explicit stop, controller lease, permission revocation.
- Real NAT/network/device sleep matrix and bandwidth testing.

**Gate:** the mobile user handles an OS UI prompt, minimizes the preview and continues the same Agent conversation; no capture remains active after stop.

## Milestone 5 — Product completeness and extensibility

- Agent-provider ecosystem and flexible handoffs; worktree workflows.
- Plugin development kit, package signing, controlled distribution and registry.
- Search, session export, shareable deep links, reusable layouts, automation/jobs, webhook integrations.
- Windows/Linux Desktop Host parity; headless servers; multi-machine aggregation.
- Android Compose; optional Web and Ratatui clients using same domain protocol.
- Accessibility, localization, analytics opt-in, perf budgets, E2EE review, backups/migrations.
- Reliable public release, security docs and contribution process.

**Gate:** complete product capability matrix with cross-platform contract tests and documented unsupported features.

## Cross-cutting engineering tracks

### Runtime and protocol

Contract tests with pinned herdr-core, graceful degradation, version negotiation, two clients on same terminal, resume after event gap, agent status provenance.

### Security

Authn/authz, pairing revocation, plugin permission isolation, secure screen control, key storage, transport encryption, audit and threat reviews are required from early milestones — not deferred to the final phase.

### Experience

Maintain the accepted Design Direction v0.1: clean native UI, Workspace-first, multiple agents with flexible panes, mobile chat-first, floating screen as a small extension.

## Proposed initial issues

1. Spike: connect to hc OpenRPC and framed terminal attach.
2. Design: app Session/AgentRun mapping and event store.
3. Prototype: GPUI window with Spaces, panes and command palette.
4. Research: Codex structured API and provider adapter contract.
5. Design: plugin manifest/capabilities/native UI slots.
6. Research: Alma relay wire protocol and pairing compatibility.
7. Security: QR handshake threat model and grants.
8. Spike: ScreenCaptureKit → WebRTC → SwiftUI floating preview.
9. DevEx: CI, tests, formatting, lint, code signing and reproducible builds.
10. Design: recover and reconcile Host with herdr-core after restart.

See [product](PRODUCT.md), [architecture](ARCHITECTURE.md), and [decisions](adr/0001-platform-and-runtime.md).
