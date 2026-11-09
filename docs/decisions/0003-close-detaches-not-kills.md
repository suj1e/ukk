# ADR-0003: Closing a view detaches; it never kills a terminal

**Status:** Accepted · 2026-11-09

## Context

The reference Herdr app ends a terminal when its pane, tab or space is closed:
`src/window.rs` routes `pane.close`, `tab.close` and `space.close` into `kill`
paths, so the UI's close action is also a process action.

ukk is built for long-running coding agents. A closed window, a switched tab or
a rearranged layout must not destroy the process a user or an Agent is still
working in, and a remote phone must be able to reattach after the Desktop
window is gone. ukk therefore keeps a non-negotiable rule: **closing a
Pane/Tab/window must never signal an Agent or terminal** (AGENTS.md).

The world structure also moved: spaces, tabs and panes now live in the
herdr-core document `herdr-app`, read and written with `doc.get` / `doc.set`
and a version CAS. A document mutation is a metadata edit of the shared world,
never a process operation.

## Decision

1. Close is **detach-only**. Closing a Pane, Tab, Space view, floating tile or
   the Desktop window changes view metadata at most; it never calls
   `pane.close`/`tab.close`/`space.close`, terminal stop/kill, or provider
   abort/stop.
2. Removing a pane or tab from the `herdr-app` document is a layout edit. The
   herdr-core terminal survives, stays addressable by its terminal address and
   is reattachable (the Herdr reference itself reattaches the same terminals
   after a reconnect).
3. Only an explicit user action with the agent-control capability may end
   execution: a typed stop/kill command through the bridge, or the provider's
   own abort with its documented semantics. Detaching a view is never a stop.
4. A future product change to kill-on-close requires its own decision record
   and explicit user approval before any code changes. It must not be adopted
   silently by copying Herdr's window behavior.

## Scope

Applies to the Desktop shell, per-pane and per-tab controls, space management,
the floating Remote Screen tile, window close and app quit, and every debug or
convenience path that removes a view. It does not restrict explicit termination
actions, which remain separate, confirmed and capability-checked.

## Consequences

- The UI needs a visible difference between "close view" and "stop process";
  close buttons detach, stop lives with explicit Agent controls.
- Layout edits must be reversible without a runtime side effect, so the
  `herdr-app` document and the local view file carry no process lifetime.
- Tests and review must treat any close path reaching a kill signal as a defect.
- After app exit, running terminals stay in herdr-core; ukk reattaches by
  observation instead of promising crash recovery the runtime does not give.
- Herdr's exact `pane.close`/`tab.close`/`space.close` behavior can never be
  matched; [herdr-app](../reference/herdr-app.md) records this as the one
  deliberate difference.

## Alternatives rejected

- **Match Herdr's kill-on-close**: destroys running agents, contradicts the
  product promise and the "no lost running agents" quality requirement.
- **Confirm-on-close dialog that kills**: still couples window management to
  process lifetime and fails when the window crashes.
- **Kill only on space close**: an arbitrary line; users reorganize spaces
  while work continues.

## References

- [AGENTS.md](../../AGENTS.md) — non-negotiable close rule
- [Technical design](../tech-design.md) — runtime and terminal rules
- [Domain model](../subsystems/domain-model.md) — view edits vs process control
- [herdr-app reference](../reference/herdr-app.md) — upstream kill paths
