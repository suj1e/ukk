# Review process

Review protects product boundaries and process lifetime, not style preferences.
Author and reviewer both use this page.

## Required evidence

- PR links the goal/task or issue, states scope and non-goals, and names the
  user-visible behavior.
- `just check` passes locally; CI is green on the exact head SHA.
- Implementation PRs add real build/test/launch gates and attach actual
  results. The current static gate is not app evidence.
- New behavior has a test that fails without the change when practical.
- Security-sensitive changes have denial tests and an explicit threat note.
- Docs/status/roadmap are updated when a claim or contract changes.
- Real-device and real-runtime claims are marked **NOT VERIFIED** unless
  evidence is attached.

## Checklist

- [ ] Scope matches the PR description; no drive-by features or widened permissions.
- [ ] Layout stays `apps/desktop` + `apps/ios`; no Rust/Cargo/crates/plugins
      return and no root or nested Go module appears.
- [ ] herdr-core owns Agent/terminal processes; view close sends no stop/kill/detach.
- [ ] Approvals/tool execution come from typed provider events, never ANSI or
      terminal-text inference.
- [ ] The bridge authorizes remote/adapter/screen connections; the relay carries
      no raw screen video.
- [ ] Pairing, Agent control and screen view/control remain separate grants.
- [ ] Capabilities are default-off and checked at the transport boundary.
- [ ] Errors are truthful; no silent retries that change semantics.
- [ ] Bounds exist for frames, queues, timelines, paths and child processes.
- [ ] Secrets, tokens and user source are absent from code, logs and artifacts.
- [ ] Tests are deterministic and do not depend on transient state.
- [ ] Docs and links are valid; unsupported platforms/features are named.
- [ ] CI evidence matches the current SHA, not an earlier push.

## Stop review immediately

- Checks or implementation gates are unverified, flaky, or described as green
  without a run.
- The change hides a failure, weakens a denial test, or retries until green.
- A pane/window/Session close can signal or terminate an Agent.
- Remote mutation, screen capture or input is enabled without explicit consent
  and a separate capability.
- Approvals or tool execution are reconstructed from terminal output.
- The relay carries raw video or unencrypted control data.
- Product non-negotiables in `docs/requirements.md` or `AGENTS.md` are changed
  without an explicit decision record.
- Credentials, tokens or private user data appear in the diff, logs or artifacts.
- The PR claims real-device acceptance without reproducible evidence.

## Decision rules

- One integration owner resolves cross-lane conflicts; reviewers do not push
  competing implementations.
- Prefer the smallest contract change that preserves existing callers; never add
  a compatibility shim without a removal date.
- An unresolved blocker means **request changes**, not approval with a follow-up.
- After merge: delete the branch/worktree and record durable evidence in
  [status](docs/status.md) or [lessons](LESSONS.md).
