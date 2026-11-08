## Intent

<!-- One reviewable change. Link the goal, roadmap task or issue. Keep draft until real entrypoints and gates exist. -->

Task/issue:
User journey:
Non-goals:

## Contracts changed

- Go Desktop (MyGo):
- iOS (SwiftUI):
- herdr-core / bridge / relay:
- Permissions / capabilities:

## Verification

<!-- Exact commands and outcomes. Redact tokens and user data. -->

- [ ] `just check` (structure/docs/config/module integrity, not an app build)
- [ ] Implementation gates added and run, if source exists (build/test/launch):
- [ ] Real runtime/device (if claimed):
- [ ] CI run link (exact head SHA):

Source committed: yes/no
Executable build: yes/no/N/A (until entrypoints exist)
CI passing: yes/no/N/A
Real-device verified: yes/no/N/A

## Safety review

- [ ] Closing a Pane/Tab/window cannot stop an Agent or terminal.
- [ ] Approvals/tool execution come from typed provider events, never terminal ANSI/text.
- [ ] The bridge authorizes remote/adapter/screen connections; the relay carries no raw video.
- [ ] Pairing, Agent control and screen view/control remain separate grants.
- [ ] No credentials, tokens or private user data in the diff or logs.

## Docs

- [ ] `docs/status.md` / `docs/roadmap.md` updated if a claim changed.
- [ ] Subsystem contract or runbook updated.
- [ ] Unsupported platforms and **NOT VERIFIED** items named.
- [ ] Branch/worktree cleanup done after merge.
