# ADR-0001: New GPUI Desktop, standalone Host, herdr-core runtime

**Status:** Accepted product direction; technical feasibility to validate  
**Date:** 2026-11-07

## Decision

Build ukk Desktop from scratch with Rust/GPUI, **not** as a herdr-app fork. Use a separate user-scoped Rust Agent Host for durable domain model, protocol, security and plugins; integrate herdr-core through its publicly documented API. iOS is built natively with SwiftUI and shares domain protocol, not GPUI views.

## Why

The product differentiator is a redesigned native multi-agent workspace and mobile companion. Runtime/process persistence is a solved capability we should reuse, but we should not inherit another UI's layout/extension constraints. Separating the Host from windows preserves sessions and permits mobile access after the window closes.

## Alternatives considered

- Fork herdr-app: faster terminal/Spaces reuse, less freedom to own product UX.
- MyGo Desktop: viable native Go UI with Web benefits, but adds Go integration complexity to Rust runtime and shared service at this phase.
- Cross-compile GPUI iOS: code sharing possibility, but native pairing, notifications, accessibility and gesture integration favor SwiftUI for the mobile companion.
- Rewrite the runtime: rejected due to duplicated process/PTY/agent orchestration and maintenance.

## Consequences

Build new GPUI pane/layout composition and protocol adapter; invest in structured Agent adapters. Desktop and iOS separately implement presentation. App-domain Session is distinct from herdr-core terminal identifiers.

## Revisit when

GPUI proves technically unsuitable for expected UI/accessibility, herdr-core API no longer supports required lifecycle, or MyGo offers materially better verified cross-platform performance.
