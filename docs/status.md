# ukk — Implementation status

> **Snapshot:** 2026-11-08, PR **#58** merged at `8b5660b5c98f6aaf380a8caa0ba94b05bc2c92cc` (native Go/MyGo Desktop + external herdr-core + SwiftUI iOS). This page records **exact evidence**, not intentions. Where an item has not been verified, it says so. The integration owner appends build, CI, runtime and device evidence as it lands; planned module paths in [tech-design](tech-design.md) are not proof that code exists.

## Product contract (fixed)

- **Desktop:** native Go application built with MyGo (native UI), macOS/Windows/Linux from one codebase. No Rust/GPUI application, no webview primary UI.
- **Runtime ownership:** external herdr-core v0.1.18 owns persistent terminals, processes and structured status. ukk never copies or rewrites it. Closing a Pane/Tab/Session/window detaches and never stops an Agent.
- **Service:** `ukk-bridge` is the background authorization and durable-metadata service for local clients, remote devices, agent adapters and screen connections. `ukk-relay` is the separate self-hostable control-plane relay.
- **Domain:** Space → Session → AgentRun; Pane/Tab is a view. No mandatory Planner/Coder/Reviewer/Tester workflow.
- **Mobile:** native SwiftUI iOS/iPadOS is active scope. Android, Web and TUI remain deferred.
- **Remote Screen:** opt-in Session tile, separate view/control grants, independent STUN/TURN media, never raw video on the relay.
- **Provider truth:** omp is the first real provider through a neutral contract. Codex/Claude contracts may be fixture-tested; no fake adapters and no ANSI-derived approvals/tools.
- **Reset:** greenfield. No data, schema, protocol or artifact migration from the earlier Rust/GPUI prototype. Its `main` (including `f033cf0`) is historical reference only.

Canonical documents: [requirements](requirements.md), [tech-design](tech-design.md), [v1 completion](goals/v1-complete.md), [implementation plan](goals/mygo-implementation.md), [security](subsystems/security.md).

## Current milestone status

This table records the **claim**, not a promise. `Pending evidence` means the work/verification result has not been recorded here yet; it does not mean the feature is permanently absent.

| Stage | Claim | Evidence boundary |
| --- | --- | --- |
| Docs / architecture reset | Active docs describe the native Go/MyGo + external herdr-core + SwiftUI structure; obsolete Rust/GPUI claims removed | Documentation change only |
| M0 Cutover + build spine | **Local macOS:** module builds, `ukk.app` packages ad-hoc with the pinned `libghostty-vt.dylib`, and the packaged app opens a native window and self-starts the bridge; CI is green on `bacec97` for macOS/Ubuntu/Windows plus launch smoke | Windows/Linux runtime interaction remains unverified even though they compile and test in CI |
| M1 Local workspace + terminals | **Local macOS:** real herdr-core v0.1.18 terminal `t1z` attached read/write and rendered in the MyGo terminal; observe/credit/resize behavior has package tests | Window-close process survival, observer denial against a real core, and Windows/Linux attach remain unverified |
| M2 Provider-neutral omp | **Local macOS:** bridge-owned omp 18.18.1 session started, accepted a prompt, streamed a completed turn and usage, and rendered in the Desktop conversation (`OK`, 13,622 tokens); long Markdown is chunked with a 64 KiB default and explicit 2 MiB expansion | Provider-emitted approvals and provider restart/resume remain unverified |
| M3 Relay + pairing + iOS | **Local + CI:** host-confirmed pairing, encrypted command dispatch, resume and revocation work through a real relay process; the iOS simulator build and its 94 unit tests pass in CI; the app renders a real SwiftUI screen | No deployed WSS relay, no WAN proof, no physical-device pairing/camera/APNs |
| M4 Remote Screen | **Local:** real avfoundation capture produced an offer with ICE/DTLS and H.264; stop ended capture; input gating has tests | No TURN media session, no injection into a real app, no iPhone viewer |
| M5 Plugin safety | Manifest/grants/supervisor/framing and a first-party moejs runtime have package tests (including timeout, import denial and no ambient globals); bridge API and a Settings surface are wired | Third-party helpers are process-isolated, not OS-sandboxed; no malicious-plugin acceptance run |
| M6 Unsigned distribution | **CI:** release run `42476394079` built and uploaded the macOS DMG, Windows NSIS installer, Linux deb/tar and unsigned device IPA; downloaded artifacts matched their `SHA256SUMS.txt` and formats locally | Unsigned IPA is non-installable; no three-OS install/uninstall/a11y matrix |

## Evidence register (2026-11-08)

| Check | Observation | Result |
| --- | --- | --- |
| Format / vet / tests | `gofmt -l .`, `go vet ./...`, `go test ./...`, `go test -race ./...` in `apps/desktop` | clean; 296 tests, 18 packages; race clean |
| Package | `go tool mygo build` for darwin/arm64, linux/amd64, windows/amd64 | macOS 20.7 MB `.app`; Linux `.deb` + tar + PKGBUILD; Windows `.exe`; terminal libraries bundled |
| Native window | packaged app launch | window renders; bridge connects |
| Bridge lifetime | quit the app, then `/v1/command` health over its socket | `{"ok":true}`; bridge kept running |
| Terminal | herdr-core v0.1.18 `t1z` attached with libghostty-vt | `ukk terminal smoke` rendered; survived app exits |
| Agent | omp 18.18.1 via bridge start/prompt | assistant `OK`, turn `done`, 13,622 tokens |
| Relay + pairing | `TestPairResumeAndCommandsThroughRelay` | host-confirmed pairing, encrypted commands, resume, revocation denial |
| Remote Screen | real ffmpeg/avfoundation capture | WebRTC offer with ICE/DTLS + H.264; stop ended capture |
| Cross-compile | `GOOS=windows` / `GOOS=linux` build and vet | clean |
| CI (`936994d`) | Actions run `42476717872` | docs/layout, Go macOS/Ubuntu/Windows, launch smoke all success |
| iOS build | Xcode-beta 27.0 generic simulator build, test build | success; `build-for-testing` success |
| iOS launch | simulator install + launch with `-ukk-fixture` | native screen renders with an explicit fixture banner |
| iOS crypto | macOS Swift probe of SecureChannel | transcript/HKDF/codes matched RFC 7748-derived values |
| iOS XCTest | CI simulator build + unit tests (run `42476718034`) | **passed**: 94 tests on an available iPhone simulator |
| Release artifacts | Release run `42476394079` (`21e8d30`, same bridge code as the merged tree) | four jobs success; desktop archives and IPA checksums re-verified locally; NSIS/DMG/IPA formats confirmed |

These are single-host observations beyond the CI run. Windows/Linux runtime GUI, a deployed relay/TURN path and physical iPhone remain **NOT VERIFIED**.

Do not copy a green result from one milestone or SHA to another. Do not treat an existing `apps/ios` fixture screen as Host integration.

## What is retained from before the reset

- **iOS:** the SwiftUI app now carries real pairing, resume, encrypted RPC, approvals and a WebRTC viewer. Fixture content is shown only when no Host is paired and is labelled as such. No physical-device camera/APNs run has been recorded.
- **Historical Rust implementation:** the previous `main` and its merged PRs are historical context only. They are not a compatibility target, are not migrated, and must not be cited as current source. Old crate/module paths and commands do not describe this tree.
- **Retained documentation:** product/architecture docs, goals, decisions, references, the domain/security/plugins subsystem docs, release/incident runbooks and release assets remain the active documentation set.

## Verification boundaries

- **Source code:** a file, package or entrypoint existing does not mean the feature works.
- **CI:** only a green workflow run on the exact SHA counts. A configured workflow or local command that was not observed is not evidence.
- **Native GUI:** window creation, keyboard/accessibility, permission dialogs and service installation require OS-specific real-window evidence.
- **herdr-core:** requires a pinned real daemon plus compatibility tests; mock transports alone are not process-survival evidence.
- **omp:** requires a real provider run through the bridge; CLI output pasted as a UI claim is not GUI evidence.
- **Relay/TURN:** requires a deployed WSS/TLS relay, operator-auth denial, E2E tamper/replay checks, revocation and actual media over STUN/TURN. A local mock relay is not WAN evidence.
- **iOS:** simulator build/tests do not prove camera, APNs or physical-device pairing. Signing/device prerequisites must exist before claiming PASS.
- **Artifacts:** checksums and `--version` identity must match the exact SHA; an unsigned IPA is non-installable.

## Safety invariants under test

- No Pane/Tab/Session/window close sends stop/kill; only explicit user intent with agent-control may stop execution.
- Terminal input is never byte-sniffed; the current single plugin write path uses the legacy DATA tag for both human keys and automatic replies (documented limitation), while observers remain read-only.
- Observers never write or resize; controller leases are explicit and expiring.
- RESIZE is applied before paint; backpressure is bounded.
- No ANSI-to-approval/tool inference.
- Pairing, agent control, screen view and screen control are separate grants.
- Relay carries control frames only; screen media uses independent STUN/TURN.
- Third-party plugin code does not run in the privileged bridge process by default.
- Screen capture/input has visible consent, a one-tap stop and revoke teardown.

## Open acceptance gaps and environment blockers

Everything below is **BLOCKED on external environment or credentials**, not on missing source. The implementation and CI evidence above must not be read as acceptance for these items.

| Blocked acceptance item | Why it cannot run here | What would unblock it | Current evidence boundary |
| --- | --- | --- | --- |
| Windows and Linux native GUI/runtime matrix | No physical Windows/Linux machine or desktop VM with a display, GPU and OS permissions is attached to this session | A Windows 11 host and a Linux desktop (X11/Wayland) with the packaged artifacts; run window, terminal, pane/tab, Agent view and uninstall checks | CI compiles, tests and smoke-launches macOS only; Windows/Linux runtime interaction is **NOT VERIFIED** |
| Deployed WSS relay + TURN WAN acceptance | No domain, TLS certificate, public relay deployment, TURN credentials or second network | Deploy `ukk-relay` behind TLS, configure TURN, then exercise pairing, resume, revocation, operator-auth denial and reconnect across two networks | Local in-process relay routing and pairing tests only; no WAN or media-over-TURN proof |
| Physical iPhone camera/pairing/APNs/floating screen | No Apple signing identity, provisioning profile, physical iPhone or APNs key | Sign a device build, install on a physical iPhone, scan the real QR, approve a command and open Remote Screen over the deployed media path | Simulator build and 94 unit tests pass; device camera, APNs and physical pairing are **NOT VERIFIED** |
| OS-level third-party plugin sandbox | The standard Go toolchain and this session provide no portable OS sandbox; helpers are process-isolated with cleared environment, bounds, deadlines and kill-on-disable | Per-OS sandbox design and implementation (macOS App Sandbox, Windows AppContainer/job objects, Linux namespaces/seccomp or equivalent) plus a malicious-plugin acceptance run | Documented as **process isolation, not an OS sandbox**; no malicious-plugin containment claim |

The remaining ordered implementation work, if any, stays in [mygo-implementation](goals/mygo-implementation.md). Do not mark a blocked row as passing by inference from CI.

## Maintenance

One integration owner updates this page after merges and acceptance runs. Record exact SHA, OS/device, command or workflow URL, artifact hash and observed result. Mark **NOT VERIFIED** or **BLOCKED** rather than inferring success. Milestone stages are implementation order, not user-confirmation gates.
