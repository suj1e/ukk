# Native GPUI Split Drag — Spike 06

> 2026-11-07 · Implementation in `feat/gpui-drag-resize`. This is a native splitter interaction slice, not complete Tabs, PTY attachment, or full coding-agent features.

## Behavior

- Each persisted `PaneNode::Split` renders a thin, cursor-sensitive **8px draggable separator**, horizontally or vertically according to its split axis.
- On GPUI drag move, ukk calculates a local view-only ratio from the split container's actual bounds, clamped to the same 15%–85% constraint already enforced by `ukk-domain`.
- Both child widths/heights and the percentage display update **during the drag**. The preview does not affect SQLite or herdr-core.
- On mouse release/drop, Desktop sends **one** authenticated local `workspace.pane.resize` RPC with the current `expected_revision`.
- The Host persists with SQLite compare-and-swap and returns a new Workspace snapshot, which also flows to other windows via the durable `workspace.wait` subscription introduced in PR #8.
- Concurrent layout edits can be rejected as a revision conflict; the user can reload the latest layout. No silent overwrites.
- The previous “+ / −” resize controls remain usable as a keyboard/mouse fallback.
- Keyboard users can focus the splitter (visible focus ring, `Splitter` role with orientation and 15–85% value) or use `Ctrl+Alt+Arrow` to resize the enclosing split without a pointer. See [Keyboard, accessibility and local focus](GPUI_KEYBOARD_A11Y.md).

## Structure

```text
apps/desktop/src/workspace/
  mod.rs            Workspace state + per-drag transient preview identity
  resize.rs         ratio math, drag preview, completion and pure unit tests
  panes.rs          leaf Pane contents and recursive dispatch
  splits.rs         draggable separator, split layout and native drag/drop bindings
  actions.rs        local Host mutation, never Agent lifecycle management
  sidebar.rs        Session/Agent navigation cancels a transient drag
```

Only layout presentation changes: `Pane` is not a terminal, AgentRun or execution owner. Closing a Pane must never terminate an Agent.

## Acceptance

- [x] Source: real horizontal/vertical mouse drag handles.
- [x] Source: transient local preview and single Host write at gesture completion.
- [x] Source: prevent cross-Split drag interference and clear previews on navigation.
- [x] Pure ratio tests: clamp and reject invalid geometry.
- [ ] Root `cargo fmt --check`, `cargo test --workspace`, strict Clippy on macOS / Windows / Linux.
- [ ] GPUI native `cargo check`, **Desktop unit tests and strict Clippy** on all three desktop operating systems.
- [ ] Actual GUI smoke on macOS/Windows/Linux: drag nested horizontal and vertical splits, release inside/outside bounds, restart Desktop, verify saved ratio and no Agent stop.
- [x] Source: keyboard splitter resize and accessibility labels/value metadata.
- [ ] Check Windows/Linux mouse cursors, focus behavior, keyboard resizing and accessibility labels in a real window.

The CI build gate is not a replacement for launching a real window. Real-world interaction and accessibility acceptance remain required before release.

See [Engineering Standards](ENGINEERING.md), [Workspace Persistence](WORKSPACE_PERSISTENCE.md), and [Workspace Live Sync](WORKSPACE_LIVE_SYNC.md).
