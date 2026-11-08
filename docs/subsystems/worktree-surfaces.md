# Read-only repository surfaces (D3 slice)

> Status: **source + local macOS checks**; native GUI and three-OS CI are recorded on the PR.
> Conversation and terminal attachment are intentionally **not** part of this increment.

This document covers two pieces:

- `crates/worktree` — bounded, read-only Git inspection for one project root.
- `apps/desktop/src/workspace/repository*` — Session Files / Diff / Activity panes fed by that crate.

## Why a separate crate

The Desktop must show real repository state without growing SQL, RPC or process-control responsibilities, and without a new native dependency. `ukk-worktree` shells out to the user's `git` executable with four read-only subcommands and returns typed data. It is usable from tests and future Host-side features, but this increment only wires it into the Desktop.

## Contract

```text
Worktree::open(project_root)            -> typed error | Worktree
Worktree::status()                      -> Status   (branch, HEAD, changed files)
Worktree::changed_files()               -> Vec<StatusEntry>
Worktree::diff(DiffScope)               -> Diff     (bounded unified diff)
Worktree::activity()                    -> Activity (recent commits)
```

`DiffScope` is one of `Head`, `Staged`, `Unstaged`, `StagedFile(path)`, `UnstagedFile(path)`. File scopes use repository-relative paths exactly as Git reports them.

`Limits` defaults: 8 s per process, 512 KiB per captured stream, 2000 status entries, 20 commits. The Desktop uses `Limits::compact()` (4 s, 256 KiB, 500 entries, 12 commits).

### Typed errors

| Variant | Meaning |
|---|---|
| `NotARepository` | Missing directory or not inside a Git working tree |
| `PermissionDenied` | OS/Git access denied |
| `RepositoryNotTrusted` | Git `safe.directory` / dubious ownership refusal |
| `Timeout` | Child exceeded its budget and was killed |
| `GitMissing` | `git` could not be spawned |
| `NonUtf8Path` | Path cannot be addressed on this platform |
| `GitFailed` | Any other non-zero exit, with bounded stderr |
| `Io` | Spawn/wait failure |

Every error has `hint()` guidance; the UI copies the full diagnostic.

### Safety properties

- Only `rev-parse`, `status --porcelain=v2 -z`, `diff` and `log` are executed.
- Never `fetch`, `pull`, `checkout`, `reset`, `clean`, `add`, `commit` or any write.
- `stdin` is closed, terminal prompts disabled, no pager.
- Repository-configured `core.fsmonitor`, external diff programs and textconv filters are disabled. `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and related environment overrides are removed.
- Commands run from the repository top level and are scoped by the project-root pathspec, so a Space pointing at a subdirectory never reports sibling changes.
- Every path is sent as a *literal* pathspec (`:(literal)…`) after `--`, so `?`, `*`, `[...]`, a leading `:` and bracket directories select exactly one file. Bare pathspecs would act as globs and could show another file's diff.
- Timeouts kill the child; output readers drain past the cap so a large repository cannot deadlock the pipe.
- A byte-truncated status sets `Status.truncated`, discards any partial trailing record and never reports the tree as clean. The UI labels the result as bounded instead of claiming a clean tree.
- Only a genuinely unborn branch is an empty history or an empty `HEAD` diff. A broken ref, a ref to a missing object or an unreadable HEAD is a typed error; `show-ref` exit codes distinguish "branch has no commits" from "ref is corrupt".

**Residual risk:** Git still honours repository-configured *clean filters* declared through `.gitattributes`. A hostile repository that the user explicitly opens can execute such a filter during `status`/`diff`. This crate is not a sandbox for untrusted repositories; the UI states that inspection is local and read-only, not that the repository is trusted.

**Non-UTF-8 paths:** status is parsed from raw bytes. Display is lossy and marked in the UI; the raw bytes are preserved for single-file diff requests on Unix. Linux contract tests cover this; macOS APFS rejects arbitrary byte names, so that test is Linux-only.

## Desktop integration

`Session → Space.project_root` is the only root source. There is no new Host RPC and no mutation permission. A missing or empty root yields an explicit error state, not an empty repository.

`repository_load.rs` spawns Git work on the GPUI **background executor**, tagged with a generation counter. A late reply from a previous Session or refresh is discarded. Loading state, empty results and typed failures are distinct:

- Files: changed entries with staged/unstaged/untracked/conflict counts, branch/HEAD/upstream, rename origins. The list is capped at 200 rows; more is stated, never silently dropped.
- Diff: whole tree by default, or one tracked file in staged/unstaged mode. Preview is capped at 400 lines / 64 KiB. Untracked files show an explicit "no Git baseline" state; no diff is synthesized.
- Activity: recent commits scoped to the project root, capped and labelled when truncated.
- Every surface has Refresh and Copy state; failures add Copy diagnostics with the full typed error and hint.

### Explicit unsupported states

`unsupported_surface.rs` renders Agent conversation and Terminal panes from **real persisted identity** only:

- "Structured Agent events are not part of this build" (R1/R2 dependency).
- "Live PTY framing and attach are not wired" (R1 dependency).

No chat turns, tool calls, approvals or terminal bytes are fabricated. No pane, Session or window close path signals herdr-core.

## Verification

Local macOS (arm64, git 2.54):

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

cargo fmt --manifest-path apps/desktop/Cargo.toml --all -- --check
cargo check --manifest-path apps/desktop/Cargo.toml --features gpui_platform/runtime_shaders
cargo test --manifest-path apps/desktop/Cargo.toml --bin ukk-desktop
cargo clippy --manifest-path apps/desktop/Cargo.toml --all-targets --features gpui_platform/runtime_shaders -- -D warnings
```

`gpui_platform/runtime_shaders` is used locally because this machine has no Metal toolchain; CI uses default features. The native window was **not launched**, so real GUI rendering, selection and scrolling remain NOT VERIFIED.

Contract tests build throwaway repositories in the OS temp directory and cover: clean/dirty, staged rename, untracked, conflicts, whole-tree vs per-file diff, binary diffs, large-output truncation, status byte-cap truncation, timeout kill and reaping (pid file), missing Git, not-a-repository, unborn HEAD, corrupt/missing-object HEAD, pathspec metacharacters, subdirectory scoping and permission denial. They never run a destructive command.

## Remaining D3 scope

- Live Agent conversation and structured turns (R2, needs R1).
- Framed terminal attachment/input (R1).
- Write operations behind explicit capabilities — deliberately absent.
- Real native-window acceptance on macOS/Windows/Linux (Q0/release gate).
