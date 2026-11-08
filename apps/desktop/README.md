# ukk Desktop

Approved target: native Go/MyGo UI, external herdr-core runtime, one Go module
in this directory. MyGo and its CLI are pinned to `v0.8.3`; Go `1.27.3` is
required. The module contains the native Desktop (`cmd/ukk`), the background bridge
(`cmd/ukk-bridge`) and the self-hostable relay (`cmd/ukk-relay`), with real
herdr-core, provider, remote and Remote Screen packages under `internal/`.

The previous Rust/GPUI implementation is retained in Git history at
`f033cf08a9347d1345d18c55d98f41d3fa57e6b1`, not copied into a legacy directory.
This reset is not a working MyGo release and must not be presented as one.

## Approved ownership

Create source packages only with real implementations, not empty directories:

```text
apps/desktop/
  go.mod / go.sum / mygo.json
  cmd/
    ukk/                       native Desktop entry
    ukk-bridge/                local background entry, no GUI dependency
    ukk-relay/                 independently deployable relay entry
  internal/
    ui/
      app/                     windows, menus, theme
      workspace/               navigation, panes, tabs
      conversation/            structured messages, tools, approvals
      terminal/                native terminal view
      repository/              file and Diff views
      remotescreen/            floating Session tile
    bridge/                    background lifecycle and connection handling
    relay/                     operator authentication, routing, signalling
    agents/                    provider-neutral contract and real omp adapter
    hc/                        pinned herdr-core protocol client
    workspace/                 Space/Session identity, layout and persistence
    remote/                    pairing, authorization, encryption, reconnect
    remotescreen/              capture, independent media, input permissions
  resources/                   files shipped with the app when needed
```

`cmd/ukk` is the `mygo.json` entry. Bridge and relay share the Go module and
build without importing MyGo UI.
No root Go module, `go.work`, nested modules, `apps/shared`, or separate
`apps/bridge` / `apps/relay` projects are needed. SwiftUI stays in
[`../ios/`](../ios/).

## Current checks

From this directory:

```sh
go mod download
go mod verify
go tool mygo version
```

The app opens a native window, attaches real herdr-core terminals and talks to
the bridge for provider-neutral Agent sessions. Evidence boundaries remain in
[status](../../docs/status.md); an unsigned local build is not release
acceptance.

## Product contract

- herdr-core owns persistent terminals/processes; closing a view never stops one.
- Agent GUI uses provider protocols, not inferred ANSI/TUI approval or tool data.
- omp is the first real provider; Codex and Claude Code use the same future
  capability/event contract without fake implementations.
- Relay carries encrypted control/signalling, not raw screen frames. Remote
  Screen uses independent media and separate view/control grants.
- No user-data migration or old-Host compatibility project is required during
  this development reset.

See the [goal](../../docs/goals/v1-complete.md),
[implementation plan](../../docs/goals/mygo-implementation.md), and
[status](../../docs/status.md).
