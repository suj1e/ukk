# Contributing

Read [AGENTS.md](AGENTS.md), [REVIEW.md](REVIEW.md) and
[docs/coding-standard.md](docs/coding-standard.md) before opening a PR.

## Branch and PR

1. Claim one roadmap task or open a narrowly scoped issue.
2. Branch from current `main`; use `feat/`, `fix/`, `docs/`, `ci/` or `refactor/`.
3. Keep one reviewable intent per PR. Split unrelated refactors.
4. Describe the user journey, changed contracts and explicitly named non-goals.
5. Attach exact commands and results; CI must cover the current head SHA.
6. Request review when the gate in REVIEW.md is satisfied.
7. Use squash merge; delete the branch and any worktree afterward.

## Required checks

```bash
just check
```

This runs the documentation link/structure check, Rustfmt, workspace tests, strict
Clippy and the independent GPUI workspace check. On macOS, the release build requires
the Metal Toolchain; the CI `runtime_shaders` escape hatch is not a release artifact.

## Stop conditions

Do not request review, and stop reviewing, when any of these is true:

- a test, Clippy or CI result is missing or stale for the current SHA;
- the PR hides a failure, weakens a security test, or bypasses a capability;
- view close can stop/kill an Agent or terminal;
- remote mutation, capture or input is enabled without explicit approval;
- credentials, tokens or private user data are present;
- real-device acceptance is claimed without reproducible evidence.

See [REVIEW.md](REVIEW.md) for the full checklist and [LESSONS.md](LESSONS.md) for
known failure patterns.
