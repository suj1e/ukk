## Intent

<!-- One reviewable change. Link the roadmap task or issue. -->

Task/issue:
User journey:
Non-goals:

## Contracts changed

- Domain / SQLite:
- Host RPC / protocol:
- Desktop / iOS:
- Permissions / capabilities:

## Verification

<!-- Exact commands and outcomes. Redact tokens and user data. -->

- [ ] `just check`
- [ ] Focused test:
- [ ] Real runtime/device (if claimed):
- [ ] CI run link (exact head SHA):

Source committed: yes/no

CI passing: yes/no/N/A
Real-device verified: yes/no/N/A

## Safety review

- [ ] Closing a Pane/Tab/window cannot stop an Agent or terminal.
- [ ] No approvals/tool execution inferred from terminal text.
- [ ] New mutation/capture/input paths are capability-gated and denial-tested.
- [ ] No credentials, tokens or private user data in the diff or logs.

## Docs

- [ ] `docs/status.md` / `docs/roadmap.md` updated if a claim changed.
- [ ] Subsystem contract or runbook updated.
- [ ] Unsupported platforms and **NOT VERIFIED** items named.
- [ ] Branch/worktree cleanup done after merge.
