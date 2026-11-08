# Native Pane Tabs — incremental implementation

> 2026-11-07 · Source and tests are being reviewed in `feat/native-pane-tabs`. Compiling or passing CI is not a substitute for real-window interaction tests.

## Product contract

A Tab is a durable **view**, not an Agent or subagent process. Tabs can show Activity, Files, Diff and existing Session-bound Agent/Terminal surfaces. Closing a Tab does **not** terminate a herdr-core Agent, kill a PTY or approve any tool.

The persisted layout tree now supports three kinds of nodes:

```text
PaneNode
 ├─ Leaf { id, content }                 // old layouts remain compatible
 ├─ Tabs { id, tabs: [PaneTab...] }       // ordered view membership only
 └─ Split { id, axis, ratio, first, second }
```

`PaneTab { id, title, content }` has a unique ID and stable order. Adding a Tab to an existing Leaf preserves the original Leaf ID as the first Tab's ID, with a new Tabs container ID. Removing one of the last two Tabs collapses the group to a legacy Leaf. The last visible Pane is never closed.

**Active/focused Tab is per device.** Clicking an existing tab never writes SQLite or changes another window's focus. The selected tab falls back to a visible member if a remote edit removes it. Selected Session, Pane and Tab now persist across application restarts in a Desktop-local settings file under the user profile; see [Keyboard, accessibility and local focus](desktop-keyboard-a11y.md).

## Mutations and security

```text
GPUI Tabs UI → HostClient.local_workspace_mutation
    → private Unix Socket / Windows Named Pipe
    → local rotating capability allowlist
    → workspace.tab.add | workspace.tab.close | workspace.tab.reorder
    → SQLite expected_revision CAS / durable Workspace revision
    → cross-window workspace.wait snapshots
```

All tab edits require `session_id` and `expected_revision`. `workspace.tab.add` additionally requires `pane_id`, `title` and `content`; close and reorder use `group_id`/`tab_id`. Reorder direction is `-1` or `1`. A stale revision returns an explicit conflict, never a silent overwrite.

Validation counts hidden tabs toward the global 16-surface limit, disallows duplicate IDs/empty groups/invalid titles, and checks **every** tab's AgentRun binding belongs to the same Session. The old serde shape and SQLite schema are unchanged, so existing v1/v2 layouts load without migration.

## Files

- `crates/domain/src/panes.rs`: durable `Tabs` variant and tree validation.
- `crates/domain/src/tabs.rs`: pure add/close/reorder methods and regression tests.
- `crates/store/src/layouts.rs`: existing CAS and per-Session AgentRun checks (already traverses every tab).
- `crates/host/src/workspace_rpc.rs`: local-only tab mutation dispatch.
- `crates/host-client/src/lib.rs`: explicit RPC allowlist.
- `apps/desktop/src/workspace/tabs.rs`: native horizontal Tab strip and per-window selected Tab.
- `apps/desktop/src/workspace/{panes,actions,state}.rs`: creation, remote focus reconciliation and local Host actions.

## Verification and open tasks

- [x] Source: backward-compatible Tabs domain model; add / close / reorder.
- [x] Source: native clickable tabs, client-local focus, + Files / + Diff and close/reorder controls.
- [x] Source: existing SQLite revision CAS and Host-only authenticated mutations.
- [x] Unit/integration test cases for JSON compatibility, limits, AgentRun isolation and persistence.
- [ ] Rustfmt, domain/store/Host test suite and strict Clippy across macOS / Windows / Linux.
- [ ] GPUI native compilation and Desktop unit tests across macOS / Windows / Linux.
- [ ] Real UI smoke: add/close/reorder Tab, restart app, preserve membership but not force another window's focus; nested splits; keyboard accessibility.
- [x] Source: cross-pane drag-to-move and keyboard-first reorder/focus/move navigation.
- [x] Source: per-device **restart-persistent** local Tab/Pane focus (separate from shared layout membership), with stale-target reconciliation.
- [ ] Real-window keyboard and screen-reader acceptance on macOS / Windows / Linux (see the manual checklist in [GPUI_KEYBOARD_A11Y.md](desktop-keyboard-a11y.md)).
- [ ] Plugin-provided pane/tab surfaces (requires plugin capability API).

This is an incremental step toward [Issue #10](https://github.com/suj1e/ukk/issues/10), not completion of the full Tabs roadmap. Android/Web/TUI are deferred; future SwiftUI iOS will consume the portable domain model.
