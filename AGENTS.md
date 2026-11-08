# ukk agent routing

Start here, then follow only the pages relevant to the task.

1. [Requirements](docs/requirements.md) — product scope and decisions.
2. [Technical design](docs/tech-design.md) — architecture and ownership.
3. [Coding standard](docs/coding-standard.md) — quality gate and commands.
4. [Status](docs/status.md) and [roadmap](docs/roadmap.md) — what is verified and what is claimable.
5. [Subsystem contracts](docs/subsystems/) — Host, domain, protocol, security, Desktop, omp, plugins, iOS boundary.
6. [Runbooks](docs/runbooks/) — Host lifecycle, Windows IPC, release, recovery.
7. [Review](REVIEW.md), [contributing](CONTRIBUTING.md), [lessons](LESSONS.md) — process and evidence.

## Non-negotiable

- Build a new Rust/GPUI Desktop; never fork or copy herdr-app UI.
- herdr-core owns persistent terminals/Agents. ukk Host owns product metadata and authorization.
- Closing a Pane/Tab/Session/window must never signal an Agent or terminal.
- Space, Session, AgentRun, TerminalRef and Pane are distinct entities with distinct lifetimes.
- Never infer approvals, turns or tool execution from terminal text; use typed events only.
- Remote Screen is an opt-in floating Session plugin. Pairing, Agent control and screen control are separate grants.
- Android, Web and TUI are deferred. iOS stays native SwiftUI.
- Do not widen remote/local mutation surfaces, capture or input permissions without explicit user approval.

## Work loop

1. Take one roadmap task or narrowly scoped issue; check open PRs first.
2. Create a topic branch; keep `main` releasable.
3. Read the relevant subsystem contract before editing.
4. Add tests for behavior, boundaries, security and process lifetime.
5. Run `just check` (or the equivalent commands in DEVELOPMENT.md); fix failures.
6. Open a PR with exact CI evidence. Distinguish source, CI and real-device verification.
7. Update status/roadmap/runbooks when behavior or a claim changes.
8. Merge only after the gate in REVIEW.md passes; delete the branch.

## Never

- Never fabricate test output, CI status, screenshots or real-device acceptance.
- Never add placeholder implementations that claim a feature works.
- Never commit credentials, tokens, private keys or user source.
- Never mask a failing or flaky check by retrying until green.

Product decisions live in [docs/requirements.md](docs/requirements.md); architecture in [docs/tech-design.md](docs/tech-design.md). If they conflict with this page, the product/architecture documents win.
