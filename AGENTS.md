# ukk agent routing

Start here, then follow only the pages relevant to the task.

Follow the official MyGo documentation for native UI, terminal, packaging and
platform behavior: <https://mygo.egoist.dev/docs>. Pin API and behavior facts to
the MyGo version recorded in `apps/desktop/go.mod`; the local module source and
`go doc` are the authority when the website and the pinned release differ.

0. [Goal](docs/goals/v1-complete.md) and [implementation plan](docs/goals/mygo-implementation.md) — one delivery track, issue #55.
1. [Wire contracts](contracts/README.md) — versioned agent/bridge/core/remote schemas and fixtures. Change the contract here first.
2. [Requirements](docs/requirements.md) — product scope and decisions.
3. [Technical design](docs/tech-design.md) — architecture and ownership; [Herdr reference](docs/reference/herdr-app.md) records the official app's model and metrics.
4. [Coding standard](docs/coding-standard.md) — quality gate and commands.
5. [Status](docs/status.md) and [roadmap](docs/roadmap.md) — what is verified and what is claimable.
6. [Subsystem contracts](docs/subsystems/) — workspace identity, security and plugin boundaries.
7. [Runbooks](docs/runbooks/) — release and incident handling.
8. [Review](REVIEW.md), [contributing](CONTRIBUTING.md), [lessons](LESSONS.md) — process and evidence.

## Non-negotiable

- Build Go/MyGo native Desktop, native SwiftUI iOS, and use external herdr-core; never fork herdr-app.
- All product code lives in apps/desktop or apps/ios. The single Go module and MyGo config live in apps/desktop.
- Closing a Pane/Tab/Session/window must never signal an Agent or terminal.
- Space, Session, AgentRun, TerminalRef and Pane are distinct entities with distinct lifetimes.
- Never infer approvals, turns or tool execution from terminal text; use typed events only.
- Remote Screen is an opt-in floating Session plugin. Pairing, Agent control and screen control are separate grants.
- Android, Web and TUI are deferred. iOS stays native SwiftUI.
- Bridge and relay share the Desktop Go module but must not import the GUI. Relay carries control/signalling, not video.
- This is a development reset: no user-data migration or old Rust Host compatibility layer.
- Do not widen remote/local mutation surfaces, capture or input permissions without explicit user approval.

## Work loop

1. Take one roadmap task or narrowly scoped issue; check open PRs first.
2. Create a topic branch; keep `main` releasable.
3. Read the relevant subsystem contract before editing.
4. Implement behavior before creating packages. No empty mains, fake adapters or no-op tests.
5. Run `just check` for current structural/dependency gates. Each feature PR must add real build, behavior and launch evidence.
6. Open a PR with exact CI evidence. Distinguish source, CI and real-device verification.
7. Update status/roadmap/runbooks when behavior or a claim changes.
8. Merge only after the gate in REVIEW.md passes; delete the branch.

## Never

- Never fabricate test output, CI status, screenshots or real-device acceptance.
- Never add placeholder implementations that claim a feature works.
- Never commit credentials, tokens, private keys or user source.
- Never mask a failing or flaky check by retrying until green.

Product decisions live in [docs/requirements.md](docs/requirements.md); architecture in [docs/tech-design.md](docs/tech-design.md). If they conflict with this page, the product/architecture documents win.
