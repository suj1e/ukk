# Goal: v1 product completion (unsigned distribution)

Status: active · 2026-11-08. This is the completion contract for ukk v1, not a
feature wish list. A milestone is complete only when its acceptance evidence is
recorded; source presence is not completion.

## Definition of done

### 1. Provider-neutral Agent runtime

- One stable Host/Desktop contract for Agent sessions, turns, tools, approvals,
  capabilities, errors and bounded timelines.
- The first real provider is **omp**; it must use the same contract as future
  providers.
- Codex and Claude Code remain adapter targets, not forks of the Host or
  Desktop. Their contracts may be fixture-tested before a real provider run.

### 2. Local Agent workspace

- Start, prompt, steer, abort, stop, observe and approve from the Desktop with
  explicit user intent and the separate agent-control capability.
- Closing a Pane/Tab/Session/window never stops an Agent or terminal.
- Terminal: full interaction (input, paste, IME, resize, named keys) or a
  documented product-level restriction; no inferred Agent events from text.
- Files/Diff/Activity and conversation panes show real data; no synthetic chat.
- Space/Session/AgentRun management is usable without the CLI.

### 3. Remote companion

- WSS/TLS relay with operator authentication, TURN, WAN proof-of-possession and
  device revocation.
- Native iOS/iPadOS app with real QR pairing, Host connection, chat, approvals,
  APNs and on-device camera.
- Remote Screen inside a Session: separate view/control grants, real media
  transport, explicit consent, secure stop, floating preview and temporary
  expansion.
- Windows/Linux capture adapters or an explicitly unsupported state.

### 4. Extensibility and safety

- Third-party plugin trust boundary, OS sandbox where available, durable grants
  and audit, capture/input permissions and revocation.
- Threat review covers local IPC, relay, pairing, plugins, screen and terminal
  input. Denial and replay tests are required for every new mutation surface.

### 5. Unsigned distribution artifacts

Final release candidates are **unsigned** and clearly labelled:

| Platform | Artifact | Contents |
| --- | --- | --- |
| macOS | `.dmg` | unsigned `ukk.app`, Host, README/licence, install notes |
| Windows | `.msi` (or NSIS `.exe`) | Host, Desktop, Start Menu entry, uninstaller |
| Linux | `.deb` (plus AppImage or tar) | Host, Desktop, `.desktop` entry, uninstall metadata |
| iOS | `.ipa` | unsigned Payload/`UkkCompanion.app`, device build, no signing |

An unsigned iOS IPA is an artifact, not an installable App Store/TestFlight
release; it requires later signing. Signing, notarization and App Store
distribution are explicitly out of scope for this goal.

### 6. Acceptance

- macOS, Windows and Linux: real window, keyboard/a11y smoke, Agent survive
  Desktop close, Host restart/reconnect, install/uninstall.
- Physical iPhone: pairing, reconnect, approval, screen preview/control.
- Every unsupported combination is named in docs/status.md and docs/roadmap.md.

## Milestones

| ID | Milestone | Exit |
| --- | --- | --- |
| G0 | Provider contract + packaging pipeline | ADR, generic `agent.*` contract with omp, unsigned installer/IPA workflow |
| G1 | Local control + full terminal | Desktop start/prompt/abort/stop/approve, terminal interaction, Codex/Claude contracts |
| G2 | Remote transport + iOS | WSS/TLS relay, TURN, physical iPhone pairing/chat/approvals/APNs |
| G3 | Remote Screen | Real media/control, grants, floating overlay, capture adapters |
| G4 | Plugins | Third-party sandbox/grants/audit and revocation |
| G5 | Release acceptance | installers/IPA, three-OS and iPhone matrix, docs/status |

## Non-goals

- Android, Web and TUI applications.
- Signing, notarization, App Store/TestFlight distribution.
- Rewriting herdr-core or forking herdr-app.
