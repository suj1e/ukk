# Lessons with evidence

Append a lesson only when it is backed by a merged fix, CI run or real-device
observation. Keep the queue short; promote stable rules into
[coding-standard.md](docs/coding-standard.md) or [REVIEW.md](REVIEW.md).

| Date | Lesson | Evidence | Durable action |
| --- | --- | --- | --- |
| 2026-11-08 | Windows text-mode Python writes emitted CRLF in `<archive>.sha256`, so `shasum -c` failed after download. | [PR #46](https://github.com/suj1e/ukk/pull/46); Windows artifact verified with LF only. | Write checksum bytes with explicit `\n`; release CI rejects `\r`. |
| 2026-11-08 | A mock provider test asserted `running` immediately and raced a fast `complete`. | macOS CI failure on PR #43; fixed by synchronizing on a streaming event. | Never assert transient state without a gate; test observable transitions. |
| 2026-11-08 | An omp stdout reader failure marked the child exited but did not kill/reap it. | Security review PR #43; fixed with idempotent `kill_and_reap`. | Every reader/writer/startup failure path must terminate and reap owned children. |
| 2026-11-08 | Prompt admission checked `Idle` and set `Running` under separate locks, so two clients could both submit. | Review finding PR #43; fixed with one atomic CAS and bounded request-id idempotency. | Admission checks and state changes belong in one lock/CAS. |
| 2026-11-07 | Pane/tab worktrees and squash-merged branches stayed after PR merge because `git branch --merged` does not see squash commits. | Branch cleanup audit; all 39 remote heads mapped to merged PRs. | Clean by PR `headRefName`, not ancestry; remove worktrees explicitly. |
| 2026-11-07 | GPUI release builds fail without the Metal Toolchain; `runtime_shaders` is only a CI smoke escape hatch. | `docs/release.md`; three-OS release packaging requires the default build. | Never publish or promote `runtime_shaders` binaries. |

## Adding a lesson

- State the failure, not the intended design.
- Link a PR, CI run or reproducible command.
- Add the smallest durable action; delete the row once a rule or test enforces it.
