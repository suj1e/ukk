# Keyboard, accessibility and local focus lifecycle

> 2026-11-08 · Desktop-only slice for roadmap task **D2**, related to
> [Issue #6](https://github.com/suj1e/ukk/issues/6) and
> [Issue #10](https://github.com/suj1e/ukk/issues/10). Source and unit tests
> are implemented; real-window screen-reader acceptance is **NOT VERIFIED**.

## Product contract

Keyboard input is a first-class alternative to the pointer, but it never
changes the security model:

```text
keyboard shortcut → PaneAction → HostClient.local_workspace_mutation
                  → local rotating capability allowlist
                  → workspace.pane.* / workspace.tab.*
                  → SQLite expected_revision CAS
```

Shortcuts cannot start, stop, signal or approve an Agent. They cannot bypass
Host authorization, and they cannot write a guessed revision. Closing a Pane
or Tab still never terminates a herdr-core process.

Focus itself is **client-local presentation state**:

- The selected Pane and active Tab are stored per device in a Desktop-local
  JSON file under the user profile.
- They are never written to SQLite, never sent through `workspace.*` RPC and
  never broadcast to another Desktop.
- A second window or device can change shared Tab membership without moving
  this device's focus.
- Removing a focused target on the Host is reconciled locally; the Desktop
  never resurrects a deleted Pane or forces another client's selection.

## Shortcuts

Bindings are registered in `apps/desktop/src/workspace/keymap.rs` and are
active in the `Workspace` key context (the workspace root holds focus).

| Action | macOS | Windows / Linux |
|---|---|---|
| Focus next / previous Pane | `Ctrl+Tab` / `Ctrl+Shift+Tab` | same |
| Select next / previous Tab | `Ctrl+PageDown` / `Ctrl+PageUp` | same |
| Move keyboard focus forward / back | `Tab` / `Shift+Tab` | same |
| Add Tab in focused Pane | `⌘T` | `Ctrl+T` |
| Close active Tab | `⌘W` | `Ctrl+W` |
| Move active Tab left / right | `Alt+←` / `Alt+→` | same |
| Move active Tab to previous / next Pane | `Alt+Shift+←` / `Alt+Shift+→` | same |
| Split focused Pane horizontally / vertically | `⌘D` / `⌘⇧D` | `Ctrl+D` / `Ctrl+Shift+D` |
| Close focused Pane | `⌘⇧W` | `Ctrl+Shift+W` |
| Shrink / grow enclosing split | `Ctrl+Alt+←/↑` / `Ctrl+Alt+→/↓` | same |
| Reload layout after a conflict | `⌘⇧R` | `Ctrl+Shift+R` |

Panes are traversed in visual depth-first order. The same order is used when a
Tab is moved to an adjacent Pane. Split resize is clamped to the domain's
15%–85% range in 5% steps, and writes only once per key press.

## Focus persistence and reconciliation

`apps/desktop/src/workspace/focus_store.rs` owns a versioned schema:

```json
{
  "version": 1,
  "selected_session": "…",
  "sessions": {
    "session-id": {
      "pane": "pane-or-group-id",
      "tabs": { "group-id": "tab-id" }
    }
  }
}
```

Storage locations:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/ukk/desktop-focus.json` |
| Windows | `%APPDATA%\ukk\desktop-focus.json` |
| Linux | `$XDG_CONFIG_HOME/ukk/desktop-focus.json` or `~/.config/ukk/desktop-focus.json` |

`UKK_DESKTOP_SETTINGS` overrides the path for isolated tests. Writes are
asynchronous, size-limited, atomically renamed into place and generation
guarded. A file written by a newer Desktop version is reported and **not
overwritten**.

After every Host workspace snapshot:

1. Stale Session, Pane, Tab-group and Tab IDs are dropped.
2. A removed focused Pane moves focus to its next visible neighbour, then the
   previous one, then the first visible Pane.
3. Closing the active Tab prefers its right neighbour, then its left neighbour.
   A two-Tab group that collapses to a Leaf keeps focus on the surviving view.
4. A Tab moved across Panes focuses the moved Tab in its destination.
5. Only then is the reconciled focus persisted, and only locally.

Restart restores the device's last Session, Pane and Tab only while those
targets still exist on the connected Host.

## Stale CAS conflicts

If the Host rejects a mutation with RPC `-32031` (layout revision conflict),
the Desktop does **not** retry automatically and does not rewrite the layout.
A visible status strip explains that the change was not applied and offers:

- **Reload layout** — fetch the authoritative snapshot.
- **Retry change** — reload first, then re-issue the same intent exactly once.
- **Dismiss** — leave the layout unchanged.

Non-conflict errors and local focus-file failures use the same strip without a
retry button. The retry intent is discarded if the reload itself fails.

## Read-only terminal surface

The Terminal pane and the Agent detail terminal card attach only after an
explicit pointer/button action and expose no keyboard input path. Keyboard
focus still reaches the pane shell (split, tab, close, resize and copy
actions), but no shortcut is forwarded to the PTY. A local detach closes this
view's stream only; it never signals the terminal. See
[GPUI_TERMINAL.md](desktop-terminal.md).

## Accessibility

- Tabs expose `Tab`/`TabList` roles, `aria-selected`, titles and shortcut hints.
- Panes expose `Pane`/`TabPanel` roles with headings and descriptions.
- Splitters expose `Splitter`, orientation and 15–85 percent value bounds.
- Space, Session, terminal, Tab and action rows are real tab stops with
  `focus_visible` rings, so keyboard focus is always visible.
- Pane controls use descriptive labels ("Close this pane", "Move the active
  tab left") instead of glyph-only names; shortcut keys are announced.
- Enter/Space activation is GPUI's built-in click-on-key-up for role `Button`/
  `Tab` elements. ukk does not register a second key handler, so one key press
  cannot apply a mutation twice.
- The runtime inspector lists the full keyboard map and the local focus file.
- The root exposes an `Application` role labelled "ukk Agent Workspace".

## Small-window behavior

The root layout reads the real viewport width:

| Width | Layout |
|---|---|
| ≥ 1080 px | Sidebar + content + runtime inspector |
| 820–1079 px | Sidebar + content (inspector hidden) |
| < 820 px | Narrow sidebar + content |

Pane headers and shortcut rows wrap; the Tab strip scrolls horizontally; the
inspector scrolls vertically; split ratios stay clamped. Session content keeps
at least ~320 px after the sidebar at supported widths.

## Tests

Desktop binary tests (`cargo test --manifest-path apps/desktop/Cargo.toml
--bin ukk-desktop`) cover:

- focus persistence round-trip, replacement of an existing file, size limits and user-profile paths;
- newer-schema rejection before typed parsing, and load-before/after-snapshot adoption order;
- local-only payloads containing no `expected_revision`/`auth` fields;
- stale focused Pane/group/Tab reconciliation and neighbour recovery;
- deferred focus after Tab close, collapse, add and cross-Pane move;
- remote removal of the active Tab selects the right/left neighbour, including two-Tab collapse;
- shortcut registration uniqueness, parseability and action mapping;
- split resize clamping and deepest enclosing-split selection;
- responsive width thresholds and content-width bound.

## Manual acceptance checklist (NOT VERIFIED in CI)

Run on each of macOS, Windows and Linux with a real window. Do not infer these
results from unit tests.

1. Launch Desktop, focus the window, press `Ctrl+Tab` / `Ctrl+Shift+Tab`; the
   accent Pane ring moves in visual order and wraps.
2. `Tab` / `Shift+Tab` reach sidebar rows, Tabs, Pane buttons and splitters;
   the focus ring is visible at every stop and never disappears off-screen.
3. Add, close and reorder Tabs with shortcuts. Confirm Agent terminals keep
   running and no process stops (check herdr-core terminal list before/after).
4. Move a Tab to another Pane with `Alt+Shift+←/→`; focus follows the moved
   Tab. Close a Pane with `⌘⇧W`/`Ctrl+Shift+W`; focus lands on a neighbour.
5. Resize a split with `Ctrl+Alt+arrows`; ratio stays within 15–85% and
   persists after restart. Drag-resize still works.
6. Select a Tab, quit Desktop, relaunch; the same Session/Pane/Tab is focused.
   Confirm the file exists at the documented per-OS path.
7. Open two Desktops on the same layout. Change focus in one; the other's
   selection does not move. Delete the focused Tab/Pane from the other window;
   the first window reconciles without crashing and chooses a neighbour.
8. Force a CAS conflict (edit layout in another window, then use a stale
   shortcut); confirm the banner appears and `Retry change` applies after
   reload, never silently.
9. Shrink the window below 1080 px and 820 px; inspector/sidebar adapt, no
   clipped controls, shortcuts still routed.
10. Activate a focused Tab and Pane button with Enter and Space; each press
    performs exactly one Host mutation.
11. Screen readers: VoiceOver (macOS), Narrator (Windows), Orca (Linux).
    Confirm Tab/Pane/Splitter roles, selected state, labels and shortcut
    announcements. Record actual output; this remains NOT VERIFIED until done.

## Not implemented / out of scope

- Cross-device focus synchronization (explicitly rejected).
- User-remappable keybindings and a keymap editor.
- Plugin-provided Pane surfaces and their accessibility metadata.
- Remote screen control, Android/Web/TUI.
- Voice control actions beyond native Click handlers.
