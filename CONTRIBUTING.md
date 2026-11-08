# Contributing

Read [AGENTS.md](AGENTS.md), [REVIEW.md](REVIEW.md) and
[docs/coding-standard.md](docs/coding-standard.md) before opening a PR.

## Branch and PR

1. Claim one roadmap task or open a narrowly scoped issue.
2. Branch from current `main`; use `feat/`, `fix/`, `docs/`, `ci/` or `refactor/`.
3. Keep one reviewable intent per PR. Split unrelated refactors.
4. Describe the user journey, changed contracts and explicitly named non-goals.
5. Attach exact commands and results; CI must cover the current head SHA.
6. Keep the reset PR a draft until real Desktop/bridge/relay entrypoints and
   executable gates exist.
7. Request review when the gate in REVIEW.md is satisfied.
8. Use squash merge; delete the branch and any worktree afterward.

## Required checks

```bash
just check
```

This validates repository layout (apps only `desktop`/`ios`, Go metadata only
under `apps/desktop`, no Rust/Cargo/crates/plugins), required documentation and
relative links, the exact `mygo.json` contract, and agreement between the MyGo
and iOS versions. It also checks `go.mod`/`go.sum` integrity.

It does **not** compile, test or launch anything until real entrypoints and
executable gates land. The implementation PR that adds those entrypoints must
add and run the real build/test/launch gates in that same PR and attach their
actual output. Never add dummy `main` files, empty packages or no-op recipes to
make a gate look green.

## Stop conditions

Do not request review, and stop reviewing, when any of these is true:

- a check or implementation gate is missing or stale for the current SHA;
- the PR hides a failure, weakens a security/denial test, or bypasses a capability;
- closing a view can stop or kill an Agent or terminal;
- approvals or tool execution are inferred from ANSI or terminal text;
- remote mutation, capture or input is enabled without separate explicit consent;
- the relay carries raw screen video or credentials/private user data;
- real-device acceptance is claimed without reproducible evidence.

See [REVIEW.md](REVIEW.md) for the full checklist and [LESSONS.md](LESSONS.md) for
known failure patterns.
