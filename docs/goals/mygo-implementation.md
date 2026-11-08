# MyGo implementation plan — M0…M6

Status: active · 2026-11-08. Ordered implementation and evidence plan for the native Go/MyGo Desktop + external herdr-core + Go bridge/relay + SwiftUI iOS product. These stages define engineering order and acceptance evidence; they are not user-confirmation gates and do not narrow the product scope in [requirements](../requirements.md) or [v1-complete](v1-complete.md).

No calendar deadlines are promised. A stage is complete only when its **evidence gate** is recorded with the exact commit SHA, OS, commands/artifacts and observed result. Source presence, a configured workflow, a compiled binary or a fabricated device report is not evidence.

## Cross-cutting rules

- One Go module at `apps/desktop`, with `go.mod`, `go.sum` and `mygo.json`; entrypoints `cmd/ukk`, `cmd/ukk-bridge`, `cmd/ukk-relay`; implementations under `internal/`.
- External herdr-core v0.1.18 is the terminal/process authority. Never copy or rewrite it.
- No data/schema/protocol migration from the Rust/GPUI prototype.
- No dummy `main`, no-op package, fake adapter, placeholder test, mock shipped as real, or claimed green check.
- Closing any UI detaches. Only explicit user intent with the agent-control capability stops execution.
- Never parse ANSI into approvals/tool events. Provider semantics only; unsupported features are labelled.
- Relay never carries raw video. Screen media uses independent STUN/TURN.
- Pairing, agent control, screen view and screen control are separate grants.

## M0 — Repository cutover and real build spine

**Intent:** make the fixed structure the only buildable source tree and prove a real native app launches.

**Work:**

1. Reduce `apps/` to `desktop` and `ios`; keep iOS fixture code as UI scaffolding, not Host integration.
2. Remove the Rust/GPUI app, obsolete crates, plugins, root Cargo metadata, old Go metadata, build scripts and workflows other than `ci.yml` and `ios.yml`.
3. Create the single Go module and exact `mygo.json` (`name`/`slug` ukk, identifier `dev.ukk.desktop`, version `0.1.0`, main `./cmd/ukk`, macOS minimum 13.0, ad-hoc signing `-`, Linux webview disabled).
4. Pin MyGo v0.8.3 as the Go tool and Go 1.27.3 as the toolchain.
5. Implement a real MyGo native shell in `internal/ui/app`: window lifecycle, navigation skeleton, menus, error/offline states. No webview page as the primary UI.
6. Implement `ukk-bridge` and `ukk-relay` entrypoints with real startup/configuration and honest failure behavior; their feature wiring lands in later stages rather than as no-op placeholders.
7. Add CI for format/vet/test/build on macOS, Windows and Linux, race tests on supported hosts, plus the existing iOS build/test workflow.

**Evidence gate:** `gofmt -l .` empty; `go vet ./...`, `go test ./...` and `go test -race ./...` pass with real assertions on hosts where the race detector is supported; `go tool mygo build` produces a packaged app whose native window opens on macOS and builds on Windows/Linux; bridge and relay binaries build and show configuration/help rather than pretending to serve; CI green on the exact SHA; no Rust build path remains in the active tree.

**Not complete if:** the app is a web page or empty window stub, the old Cargo/GPUI build is still required, any entrypoint is a dummy, or CI is green only because tests are placeholders.

## M1 — Local workspace, bridge authorization and real terminals

**Intent:** a durable local workspace whose terminals survive every UI close.

**Work:**

1. Local authenticated IPC: Unix socket on macOS/Linux, Windows named pipe, same-user peer checks, deadlines, quotas and per-connection bounds.
2. Domain persistence: Space, Session, AgentRun, layouts, revision compare-and-swap and a Desktop-owned atomic JSON store; the bridge serves a read-only mirror for remote clients.
3. `internal/hc`: herdr-core v0.1.18 client, framed attach, terminal list/observe/attach, explicit controller path, detach without stop.
4. MyGo terminal surface using the native terminal plugin (libghostty-vt through purego): observer and controller views, paste/IME/named keys, resize.
5. Provenance: one write path with source tags (`HUMAN_INPUT` / `AUTOMATIC_INPUT`); observers cannot write or resize.
6. Sizing/backpressure: apply core RESIZE before paint; bounded credit/flow control; bounded terminal ring and visible degraded state.
7. Workspace UI: Spaces, Sessions, tabs/panes, create/rename, reattach and offline/stale indicators.

**Evidence gate:** with a real herdr-core v0.1.18 on each available OS, create two terminals/agents; close the window and prove both processes remain; relaunch and reattach; observer write/resize attempts are rejected; a tagged automatic write is distinguishable from human input without byte inspection; resize is reflected before the next paint; a slow consumer hits the bounded backpressure policy rather than stalling the UI; only explicit stop with the agent-control capability ends a process.

**Not complete if:** any close path stops a process, an observer can write/resize, provenance is inferred from bytes, backpressure is unbounded, or terminal state is invented from ANSI.

## M2 — Provider-neutral Agent workspace with real omp

**Intent:** a genuinely useful local multi-Agent workspace, with omp as the first real provider.

**Work:**

1. Define the provider-neutral contract in `internal/agents`: session, turn, message, tool call, progress, artifact, approval, capability, error and bounded timeline semantics.
2. Implement the omp adapter through its RPC: bridge-owned child lifecycle, start/prompt/steer/follow-up/abort/stop, typed streaming events, bounded timeline, idempotent commands, sanitized labels, reader-failure kill/reap.
3. Desktop conversation surface: turns, tool cards, approvals only from provider events, plan/progress, files/diff/activity and error states.
4. Long Markdown: incremental, bounded parsing and virtualization; explicit truncation/expand affordance.
5. Codex/Claude: versioned fixture contracts and capability detection only. No live claim and no fake adapter.
6. Provider RPC lifetime rules: bridge restart may lose an adapter-owned child; resume is claimed only where the provider supports it.

**Evidence gate:** on a real OS, through the actual bridge/UI path, start omp, send a prompt, observe a typed tool event and assistant response, complete a turn, abort/stop and prove no child leaks. Approval allow/deny is demonstrated only if the provider emits a real approval primitive; otherwise the UI shows an explicit unsupported state and the approval gate stays open. Long Markdown remains responsive with a bounded memory profile. Contract fixtures pass for future providers; none is presented as live. Real omp GUI interaction is recorded, not inferred from CLI output.

**Not complete if:** the conversation is synthesized, approvals/tools are parsed from terminal text, an unsupported provider is faked, the timeline is unbounded, or closing the view affects the provider child.

## M3 — Remote relay, device trust and iOS companion

**Intent:** a paired phone can reach a real bridge across networks through a self-hosted relay, with real security properties.

**Work:**

1. `ukk-relay`: WSS/TLS listener, operator authentication, connection limits, opaque E2E frame routing, durable route fencing and bounded queues. No raw video.
2. `internal/remote`: short-lived QR pairing challenge, device key identity, proof of possession on every connection, E2E envelope, separate grants, rotation and revocation.
3. Bridge outbound relay registration and reconnect/replay with durable cursors; local IPC stays distinct from WAN.
4. APNs contract with opaque wake hints; no content in the push payload.
5. iOS: real QR/camera pairing, Keychain identity, connection state machine, chat, approvals where provider-supported, trusted-device management and offline/reconnect. Remove fixture labels only where a real connection exists.
6. Operator runbook for deploying relay/TLS and TURN credentials.

**Evidence gate:** two real bridge instances connect outbound through a deployed relay over WAN; a client resumes the exact Session, sends an authorized command and receives structured events; wrong operator credentials, unpaired devices, replayed frames, expired proof and revoked devices are denied; a control payload is unreadable to the relay operator; the iOS app builds and its unit/UI tests pass in simulator with an injected pairing payload. Real QR scanning, camera and APNs delivery require a physical device and are carried to M6.

**Not complete if:** the relay is mock-only, TLS/operator auth is absent, pairing implies control, revocation does not deny subsequent commands, the relay can read control plaintext, or the iOS app still claims live data from fixtures.

## M4 — Session-scoped floating Remote Screen

**Intent:** on-demand screen view/control inside a Session, with real media and platform permissions.

**Work:**

1. Floating Session tile in `internal/ui/remotescreen`; expand to viewer, minimize to the same conversation; no permanent navigation mode.
2. Separate screen-view and screen-control grants; single-controller expiring lease, visible takeover, lock/background/stop/revocation teardown.
3. Real capture: macOS ScreenCaptureKit via a permission-flow UI, Windows Graphics Capture where verified, Linux PipeWire/portal where available; unsupported combinations named.
4. Input injection behind the control grant and OS permission.
5. Real media over independent STUN/TURN with a reviewed secure transport; relay remains signalling/control only.
6. iOS overlay viewer with touch/pointer/keyboard mapping and a one-tap stop.

**Evidence gate:** real media flows over TURN with the relay carrying no video (verified by policy and traffic inspection); view without control cannot inject input; control lease expiry and a second controller are handled visibly; permission denial, stop, backgrounding and revocation tear down capture/input; each supported capture adapter is demonstrated on its OS; unsupported adapters are labelled. Physical-device media acceptance is carried to M6.

**Not complete if:** raw video traverses the relay, control is implied by view, capture continues after stop/revocation, or an unsupported capture path is presented as working.

## M5 — Plugin safety and security hardening

**Intent:** make extension and remote surfaces safe enough for third-party code without building a generic marketplace.

**Work:**

1. Versioned plugin manifest, declared commands/events/UI slots, out-of-process execution and crash containment; no privileged in-process third-party code.
2. Durable per-device grants, audit without secrets, enable/disable and revocation.
3. OS sandbox where available; otherwise a documented unsupported state and stricter supervision.
4. Threat review across local IPC, relay, pairing, plugins, screen and terminal input.
5. Denial/replay/revocation tests for every mutation surface; secret handling and log review.
6. First-party Remote Screen uses the same grant/audit primitives as other sensitive surfaces.

**Evidence gate:** a deliberately faulty/malicious sample plugin is denied privilege escalation, contained on crash and cannot forge commands; grant revocation takes effect; audit contains no tokens/secrets; sandbox behavior is demonstrated on each supported OS or explicitly marked unavailable; the threat review and tests are recorded against the exact SHA.

**Not complete if:** third-party code runs unsandboxed in the bridge by default, grants are ephemeral or unaudited, or denial/replay paths are untested.

## M6 — Unsigned distribution and final acceptance

**Intent:** ship honest unsigned artifacts and complete the verification matrix.

**Work:**

1. MyGo packaging for macOS `.dmg` (unsigned `ukk.app`, ad-hoc signing allowed), Windows NSIS `.exe`, Linux `.deb` plus tarball; include `ukk-bridge` and install/uninstall metadata.
2. Version/build identity and SHA-256 manifests; deterministic, LF-only checksums.
3. iOS device-build `.ipa` artifact without signing; label it non-installable.
4. Release runbook, rollback and artifact verification instructions.
5. Full acceptance matrix with exact evidence and explicit unsupported combinations.
6. Physical iPhone gate: pairing, reconnect, approval, APNs and screen preview/control when signing/device prerequisites exist.

**Evidence gate:** artifacts built from the exact tagged SHA; checksums and `--version` identity verified; install/uninstall and real native window/a11y smoke on macOS, Windows and Linux; Agent survival across Desktop close and bridge restart; relay/TURN deployment verified; unsigned IPA produced and labelled non-installable. The physical-iPhone gate is recorded as **PASS** only with signing, provisioning and a real device; otherwise it is **BLOCKED** with the missing prerequisite. No signing, notarization or store claim is made.

**Not complete if:** an artifact is missing, a checksum is wrong, an install path is untested, a platform result is inferred from another platform, or an unsigned IPA is described as installable.

## Evidence register

Every milestone records in [status](../status.md):

- exact commit SHA and milestone;
- OS/device and version;
- commands, workflow run URL or artifact hash;
- observed result and the test/acceptance identifier;
- an explicit **NOT VERIFIED / BLOCKED** list with the missing prerequisite.

Do not copy an earlier green result forward to a later SHA. Do not use CI compilation as a substitute for GUI, permission, service-install or physical-device evidence.
