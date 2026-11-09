# herdr-app reference

Notes from the open-source Herdr desktop app (Apache-2.0), read at
`b721ded` (2026-11). This page records how the reference is built so ukk can
match its model and look without guessing. ukk never imports or forks it.

## Two layers

1. **World model** — spaces, tabs, panes, agents. Stored in herdr-core as the
   JSON document `herdr-app` via `doc.get` / `doc.set` with a version CAS
   (`src/mirror_sync.rs:76,94`). Every client of a machine shares it.
2. **Window layout** — a JSON tree in `~/.config/herdr-app/layouts/*.json`
   describing rows/columns/stacks/blocks, sizes, pane frames, fonts and theme
   tokens (`src/layout.rs`). The default is `defaults/layouts/main.json`.

The interface itself is mods (JavaScript on gpui-kit): `spaces`, `agents`,
`tabs`, `pane`, `workspace-chrome`, `workspace-status`, `files`, `changes`.

## Default layout (`defaults/layouts/main.json`)

| Part | Spec |
| --- | --- |
| Root | row; `titlebar: none`; `background: canvas`; terminal padding 14,12; divider 1px |
| Sidebar | 268px (min 44, max 480), `chrome` background, not resizable in the file |
| Brand | 46px, `workspace-chrome`, window drag |
| Sidebar lists | column: `spaces` weight 1 min 90; `agents` weight 1 min 90 |
| Footer | 69px, `workspace-chrome` with `footer: true` |
| Tabs | 42px, `tabs` mod, window drag |
| Work body | row, padding 8, gap 8 |
| Main pane area | active tab; pane gap 8, radius 10, background `background`, border 1 `border`, focus 1 `accent`, header 30 top, mod `pane` |
| Side panel | hidden by default, 368px (min 280, max 900), stack files/changes |
| Status | 28px, mod `workspace-status` |

## Theme tokens (`src/settings.rs:300-365`)

Dark themes derive from the terminal palette:

- `canvas` = background mixed 16% toward black; `chrome` = mixed 32% toward black.
- `surface` = background; `popover` = background +6% white; `raised` +9%;
  `hover` `#ffffff0e`; `selected` `#ffffff17`; `border` `#ffffff18`;
  `input` `#ffffff24`.
- `muted` = 60% foreground, `dim` = 38%.
- `accent` = `ansi[4]`; `accent_foreground` = background.
- `working` `#e5c07b`, `blocked` `#f0705f`, `done` `#76a8ff` (or ansi
  yellow/red/blue without the override); `idle` = `dim`.
- Pane content sits on `background`; the window behind cards is `canvas`;
  sidebars, tab bar and status bar are `chrome`.

## Mods (exact behavior)

### `tabs` (103 lines)

- Bar: full height, px 10, gap 3, `chrome`, bottom border 1.
- Tab: h 29, px 10, gap 7, rounded 7, border 1 (`border` active, `chrome`
  otherwise), bg `raised` active / `chrome` inactive, 12.8px text.
  Program icon 14; title max 170 truncated; working → spinning 12px glyph;
  blocked/done → 6px dot in `blocked`/`done`; active → close button 12.
- Right side: `+` 26px opens the new-menu popup; Files/Changes toggles 26px;
  window buttons on Windows.

### `agents` (459 lines)

- Sections, highest priority first: **Needs you** (blocked/error),
  **Done**, **Working**, **Idle**; sort default by priority, optional
  "grouped by space".
- Duration since the state last changed: `<1m`, `Xm`, `Xh`, `Xd`; updated
  every 30s.
- Row: program icon, title, duration. A parent takes its most urgent
  sub-agent's place; a child without a pane is folded.
- Title: the tab name unless the terminal's live title tells something more;
  leading non-letter/number glyphs are stripped (`clean()`).
- Status comes from core terminal status (`status.state`), not terminal text.

### `spaces` (478 lines)

- Stable avatar colors per space (`ansi1..ansi6` map); name follows the first
  terminal's live folder unless named.
- Row 28/30px; repo/branch shown only where unusual; agent state lives in
  the agents list, not here.
- Spaces are grouped by machine when more than one is known; drag to reorder;
  click to switch; right-click menu.

### `pane` (75 lines)

- Header: px 10, gap 8, `background`, bottom border 1, 12px.
- Program icon 14; name max 260; folder in `dim` (home as `~`).
- working → spinner; blocked → pill `Needs you` (`blocked` at 15% alpha,
  10.5px semibold); done → 7px dot.
- Hover reveals split-right, split-down, zoom, close (14px icons); click
  focuses the pane.

### `workspace-chrome` (55 lines)

- Brand row 46: logo 20 rounded 5, name semibold 13.5, search and
  sidebar-toggle buttons 26.
- Footer 69: Settings and Add machine rows, h 28, px 8, gap 9, rounded 8.
- Sidebar collapses to a 44px rail (76 on macOS for traffic lights).

### `workspace-status` (31 lines)

- 28px, `chrome`, top border, px 14, gap 16, 11.8px muted.
- Machine (This computer), branch when known, `N agents` plus
  `· N needs you` in `blocked`, spacer, the palette key hint from the actual
  key binding.

## Connection and lifetime (`src/hc.rs`, `src/core_bin.rs`, `src/window.rs`)

- `core_bin::find` checks settings, PATH, the login-shell PATH and usual
  install dirs (`%LOCALAPPDATA%/Programs/herdr-core` on Windows), and accepts
  a candidate only if `hc --version` prints `hc …`.
- Startup connects immediately; a failed connect opens a setup window.
- A lost core reconnects every second and reattaches the same terminals
  (`window.rs:710-722`).
- Closing the window never stops a core terminal; panes stay in the core.

## What ukk must change to match

| Herdr | ukk today | Change |
| --- | --- | --- |
| World model in core `herdr-app` doc | ukk JSON store, bridge mirror | Read/write `doc.get`/`doc.set`; keep ukk metadata separate |
| Layout JSON + mods | compiled Go composition | Compile the metrics below into Go; layout files later |
| Sidebar 268 / brand 46 / footer 69 | 224 / 44 / footer | Match the metrics |
| Tabs 42, chip h29 | 34, chip smaller | Match |
| Pane radius 10, border/focus, header 30 | rail + no radius | Match |
| Status bar 28 | removed | Re-add |
| Agents four sections + state-change duration | three groups, status_ms | Match |
| Terminal pane is the default | conversation tab default | New session/tab opens a shell |
| OSC 7501 agents | now reads terminal status | Keep; add sub-agent rollup later |
