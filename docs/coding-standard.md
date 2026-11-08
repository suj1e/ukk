# ukk Engineering Standards / 工程规范

> Architecture-first baseline. These rules apply to the Go/MyGo Desktop, the background bridge, the relay, the SwiftUI companion and all plugins.

## Quality gate

A feature is **not complete** when its source merely exists. It must have a tested contract and platform-specific evidence where applicable.

1. Work on a topic branch and open a PR. Do not merge failing checks into `main`.
2. Use the pinned toolchain: **Go 1.27.3** and **MyGo v0.8.3** (`go tool mygo`, pinned in `apps/desktop/go.mod`).
3. Run from `apps/desktop`:

   ```bash
   gofmt -l .
   go vet ./...
   go test ./...
   go test -race ./...   # on hosts where the race detector is supported
   go tool mygo doctor
   go tool mygo build
   ```

   `gofmt -l .` must print nothing. `go test` must include real assertions, not placeholder tests that pass vacuously. Run the race detector on at least Linux and macOS; if a platform cannot run it, record that explicitly instead of claiming a race pass.
4. Validate macOS, Windows and Linux. Desktop window creation, native permissions and OS services differ per platform; a compile on one OS is not evidence for the others.
5. For iOS, generate the project from `apps/ios/project.yml` and run the simulator build/tests. A simulator build is not physical-device acceptance.
6. After every protocol, provider or schema change, add a fixture or contract test and update the affected docs.
7. Treat GitHub Actions run status on the exact SHA as authoritative. A source commit or a configured workflow is not proof that checks passed.
8. Keep integration tests safe by default: no real Agent runs, destructive signals, screen capture or unintended network requests unless the test explicitly opts in.
9. Never claim build/runtime/device acceptance that was not observed; write **NOT VERIFIED** with the exact missing prerequisite.

## Module boundaries

| Area | Owns | Does not own |
|---|---|---|
| `cmd/ukk` | MyGo app lifecycle, window creation, binding services | Domain rules, SQL, herdr-core parsing, remote credentials |
| `cmd/ukk-bridge` | Background service lifecycle and wiring | UI rendering, direct widget state |
| `cmd/ukk-relay` | Relay server startup, TLS/operator configuration | Product domain state, plaintext control content, screen media |
| `internal/ui/app` | Shell, navigation, menus, keybindings, settings presentation | Authorization, process lifetime, SQLite |
| `internal/ui/workspace` | Space/Session/tab/pane composition and refresh | Agent control decisions, database writes |
| `internal/ui/conversation` | Provider-neutral turns, tools, approvals, Markdown surfaces | Provider RPC parsing, approval authorization |
| `internal/ui/terminal` | libghostty-vt view, resize intent, focus | Process ownership, observer/controller authorization |
| `internal/ui/repository` | Read-only files, Git status/diff/activity surfaces | Git write actions without a capability |
| `internal/ui/remotescreen` | Floating tile, permission prompts, local presentation | Capture/input authority, media transport |
| `internal/bridge` | Local API acceptance, authorization, audit, lifecycle | Rendering, embedded provider parsing |
| `internal/workspace` | Domain identities, layouts, commands, event log, SQLite schema | herdr-core process control, UI widgets |
| `internal/agents` | Provider-neutral contract, omp adapter, capability detection | Terminal process ownership, approval replay |
| `internal/hc` | herdr-core client, framed attach, provenance tags, resize, credit | Product UI, durable metadata |
| `internal/remote` | Pairing, device identity, E2E, relay client, revocation | Screen media, generic RPC route policy |
| `internal/remotescreen` | Capture/input adapters, media negotiation, leases | Agent control, permanent navigation |
| `internal/relay` | WSS/TLS, operator auth, opaque routing, bounds | Decrypting control payloads, carrying raw video |
| `apps/ios` | SwiftUI navigation, chat, approvals, pairing, APNs presentation | Desktop rendering, shared Go code |

Keep packages **cohesive rather than arbitrarily small**. An entrypoint should stay below ~150 lines and must not become a dumping ground. If a package exceeds ~300 lines, review whether it combines independent responsibilities; split when doing so improves ownership and testability. This is a review heuristic, not a lint that encourages meaningless files.

## Go code conventions

- Name packages after domain responsibilities, not vague `utils`, `helpers` or `misc`.
- Prefer typed commands/results and wrapped errors (`fmt.Errorf("...: %w", err)`). Avoid `panic` outside startup invariants and tests.
- Do not introduce global mutable application state. Inject dependencies; isolate concurrency behind a service with explicit lifecycle.
- Every blocking operation takes a `context.Context` and honors cancellation/deadlines.
- Bound everything crossing a trust boundary: frame size, queue depth, timeline bytes, concurrency, retries and retention.
- Keep domain logic free of MyGo, platform APIs and persistence. Use interfaces at the edge, not in the middle of pure logic.
- Do not block the MyGo main thread. Event listeners and native UI callbacks run there; do RPC, disk, network and provider work on goroutines.
- Remember MyGo's threading rule: public calls are safe from any goroutine, but handler callbacks and drawing run on the main thread. Never sleep, poll or wait on a lock there.
- Use `Result`-style typed outcomes for user operations. On optimistic revision conflict, surface a retry/reload affordance; never silently overwrite shared layout.
- Keep secret keys and tokens out of logs, UI errors and audit entries. Local edit capability never authorizes terminal input, agent control, screen viewing or screen control.
- Terminal writes carry an explicit source tag (`HUMAN_INPUT` / `AUTOMATIC_INPUT`). Never inspect bytes to classify input. Observers never write or resize.
- Apply the core RESIZE result before painting. Do not paint a stale terminal grid.
- Never infer Agent state, tool calls or approvals from raw terminal text. Use structured provider events; otherwise show an explicit unsupported/terminal fallback.
- Render long Markdown incrementally and bounded; do not block the UI on unbounded parsing.
- `CGO_ENABLED=0` remains the default for MyGo builds. Do not add cgo dependencies without an accepted ADR.

## Security and correctness rules

- The bridge is the only component that authorizes remote, agent-adapter and screen connections. The Desktop does not hold remote credentials.
- Pairing, agent control, screen view and screen control are separate grants. Never treat one as implied by another.
- Screen media does not traverse the relay; the relay carries no raw video.
- Every mutation surface needs denial, replay and revocation tests.
- Closing a view detaches. Only explicit user intent with the correct capability may stop an Agent, terminal, screen stream or controller lease.
- Plugin code runs out of process by default; third-party plugins are untrusted and must be sandboxed where the OS supports it.
- Do not log bearer tokens, pairing secrets, turn credentials or screen content.

## Test pyramid

- **Pure unit:** pane split/close/resize, domain IDs, layouts, revision conflicts, source-tag selection, Markdown bounds.
- **Persistence:** SQLite reopen, migrations for the current greenfield schema, cursor retention, trust/grant revocation, idempotency.
- **Protocol/transport:** Unix socket and Windows named pipe framing, same-user acceptance, deadlines, quotas, malformed frames.
- **Provider contracts:** omp fixture transcripts plus a real gated run; Codex/Claude contract fixtures only, with no live claim until verified.
- **herdr-core:** recorded schema and mock transport tests; a pinned real-daemon acceptance job for process survival, attach/detach, resize, provenance and credit.
- **Relay/media:** TLS/operator-auth rejection, opaque frame bounds, E2E possession, revocation, TURN negotiation and no-media-on-relay checks.
- **UI integration:** Session creation → bridge → SQLite → refreshed MyGo view; closing a window does not kill an Agent; offline and error states.
- **Real OS acceptance:** native window, keyboard/a11y, permissions, service install/uninstall and bridge survival on each desktop OS.
- **iOS:** simulator unit/UI tests; physical-device pairing, camera, approvals and media only when signing/device prerequisites exist.

## PR checklist

- [ ] Focused change: clear package ownership, no unrelated rewrite
- [ ] `gofmt`, `go vet`, `go test`, race tests and `mygo build` green, or an explicit reason the platform gate does not apply
- [ ] New behavior has a failing-before / passing-after test, when feasible
- [ ] No view-triggered process kill; no hidden permission escalation
- [ ] Security review for credentials, local IPC, relay, pairing, plugins, screen and terminal input
- [ ] Schema/protocol/provider docs updated when architecture or contracts change
- [ ] Actual build/runtime outcomes recorded with exact SHA and OS; unverified items explicitly noted
- [ ] No fake adapter, dummy entrypoint, placeholder test or fabricated device evidence

## Commands

```bash
# Desktop/bridge/relay (module root apps/desktop)
gofmt -l .
go vet ./...
go test ./...
go test -race ./...
go tool mygo doctor
go tool mygo build

# Development run
go tool mygo dev

# iOS
cd apps/ios && xcodegen generate
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' build
```

The Go module is the single build boundary for Desktop, bridge and relay. The iOS app is a separate native Xcode project sharing only the protocol.
