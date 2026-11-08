# ADR-0001: Native Go/MyGo Desktop, external herdr-core, Go bridge and relay

**Status:** Accepted · 2026-11-08  
**Supersedes:** the earlier GPUI/Rust platform decision

## Decision

Build ukk Desktop from scratch as a native **Go application using MyGo** (MyGo native UI, not a webview primary interface). Use a separate Go background service, `ukk-bridge`, for durable product state and authorization of remote, agent-adapter and screen connections. Integrate **external herdr-core v0.1.18** through its documented API for persistent terminals and processes; do not copy or rewrite the runtime. Build the control-plane relay as a self-hostable Go service, `ukk-relay`. Build iOS/iPadOS natively with SwiftUI, sharing the protocol rather than UI code.

All Go code lives in one module at `apps/desktop`, with `go.mod`, `go.sum` and `mygo.json` in that directory. Planned entrypoints are `cmd/ukk`, `cmd/ukk-bridge` and `cmd/ukk-relay`; implementations live under `internal/`.

## Why

- **One native desktop codebase across macOS/Windows/Linux.** MyGo draws native UI in Go with Metal/Direct3D/OpenGL, needs no cgo and cross-compiles from any machine.
- **Real desktop behavior without an embedded-browser product.** The primary UI is MyGo native UI; webview is not the product shell.
- **Reuse the hard runtime.** herdr-core already owns PTYs, process persistence, terminal trees and structured status; duplicating it would add risk without product value.
- **Keep window lifetime separate from execution.** A background bridge plus external herdr-core means closing a window does not own or stop an Agent.
- **One language for Desktop, bridge and relay.** Typed shared contracts, simpler builds and a single Go module reduce split-brain architecture.
- **Independent mobile.** SwiftUI gives native pairing, camera, notifications, accessibility and gestures without forcing a shared UI runtime.

## Alternatives considered

- **Rust/GPUI (previous direction):** removed. It added a second language/build stack and the reset prioritizes a single Go desktop/bridge/relay implementation. Its history is reference only; no migration.
- **MyGo with a web frontend:** viable for rich documents, but the product is a native tool workspace; a webview shell would contradict the native-first requirement. Native UI keeps the terminal, chat and panes in one rendering model.
- **Electron/Tauri:** mature web UI, but adds a browser runtime and separate frontend toolchain; MyGo native UI avoids the webview dependency.
- **Fork herdr-app:** faster terminal/Spaces reuse, but inherits another UI's layout/extension constraints and does not give ukk its own product UX.
- **Reimplement the runtime:** rejected due to duplicated PTY/process/agent orchestration and maintenance.
- **Keep a Rust Host with a Go Desktop:** rejected because it preserves two build stacks and contradicts the fixed single-module Go structure.

## Consequences

- Desktop, bridge and relay share one Go module; cross-process contracts are typed in Go and versioned for the iOS client.
- MyGo's native UI and threading model become engineering constraints: no blocking the main thread, bounded rendering, terminal via the native terminal plugin (libghostty-vt through purego).
- herdr-core API compatibility is pinned to v0.1.18 for the first acceptance and must be tested against the real daemon.
- Provider RPC children are distinct from herdr-core terminal ownership; window independence is not crash recovery.
- No data/schema/protocol compatibility with the Rust prototype. This is a greenfield reset.
- Unsigned artifacts are produced by MyGo packaging plus an unsigned iOS device build; signing/notarization/store distribution stays out of scope.

## Revisit when

MyGo native UI cannot meet required cross-platform accessibility/performance, herdr-core stops supporting the required lifecycle/API, or a concrete product requirement cannot be met without a web frontend. Revisit with measured evidence, not preference.
