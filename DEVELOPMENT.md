# Development

## Current state

This branch is a clean development reset: a native Go/MyGo Desktop, external
herdr-core runtime ownership, and a native SwiftUI iOS companion. The reset
lands the layout, configuration and documentation baseline before feature code.
Until real Desktop/bridge/relay entrypoints land, `just check` validates
repository structure, documentation links, `mygo.json`/iOS version agreement
and Go module metadata. It does not build, test or launch an application, and
a green run is not app evidence. The same PR that adds executable entrypoints
must add real build/test/launch gates and update this page.

## Prerequisites

- Python 3 for the static checks (standard library only).
- `just` for the command entry points (optional; the scripts run directly).
- Go 1.27.3 when Go source lands; MyGo v0.8.3 is the pinned tool
  (`github.com/egoist/mygo/cmd/mygo`).
- herdr-core v0.1.18 as an external runtime. ukk integrates its public API and
  must not vendor, rewrite or replace it.
- `omp` for the first real structured Agent provider.
- iOS 17 SDK, Xcode and XcodeGen for the companion; see
  [apps/ios/README.md](apps/ios/README.md).
- macOS 13+ for an ad-hoc/unsigned local desktop package once packaging exists.

## Layout

```text
apps/desktop/     Go/MyGo Desktop; one Go module (go.mod, go.sum, mygo.json)
apps/ios/         Native SwiftUI companion, fixture-first
assets/           Icons and shared static assets
docs/             Requirements, design, decisions, status, runbooks
scripts/          Static layout, docs and version checks
```

`apps/desktop` is the only Go module. Planned real packages are
`cmd/{ukk,ukk-bridge,ukk-relay}` and `internal/{ui,bridge,relay,agents,hc,
workspace,remote,remotescreen}`. Add them only with real implementations, never
as empty packages or placeholder `main` files made to satisfy a command. See
[apps/desktop/README.md](apps/desktop/README.md) for ownership details.

## Commands

| Command | Purpose |
| --- | --- |
| `just check` | all static gates below |
| `just layout` | apps/metadata layout, Rust removal, single Go module integrity |
| `just docs` | required docs, relative links, `AGENTS.md` length |
| `just versions` | exact `mygo.json` contract and iOS `MARKETING_VERSION` agreement |

There are deliberately no `dev`, `build`, `test`, `launch` or `package`
recipes. The implementation PR that adds real entrypoints must add real
executable gates in the same PR and record their actual results. Extend
`just check` and CI with:

- Desktop, bridge and relay: `go build ./...`, `go vet ./...` and
  `go test ./...` (only when test packages exist), plus a headless
  bridge/relay startup smoke that needs no GUI.
- MyGo: `go tool mygo version`, real `cmd/ukk` launch checks, and unsigned
  macOS/Windows/Linux package commands derived from `mygo.json`.
- iOS: keep `.github/workflows/ios.yml` for the simulator build and tests;
  real pairing/APNs/camera/device checks remain separate explicit evidence.
- Packaging: add a real release workflow and artifact checks only after
  binaries exist; signing, notarization and store submission stay excluded.

Concrete commands once source exists include `go build ./...`,
`go test ./...`, `go tool mygo dev`, `go tool mygo package` and the iOS
`xcodebuild` commands already used by `.github/workflows/ios.yml`.

## Verification rules

- A static check is not a build, test or launch. Never present it as one.
- Run the command you changed; record the exact command and outcome in the PR.
- Mark unverified behavior and real devices **NOT VERIFIED**.
- Closing a pane, tab, window or Session never stops an Agent or terminal.
- Approvals and tool execution come from typed provider events, never parsed
  ANSI or terminal text.
- Remote mutation, capture and input need explicit, separate grants; the relay
  carries no raw screen video.
- Never commit credentials, tokens, private keys or user data.

See [coding-standard.md](docs/coding-standard.md), [status.md](docs/status.md),
the [implementation plan](docs/goals/mygo-implementation.md) and the
[security contract](docs/subsystems/security.md).
