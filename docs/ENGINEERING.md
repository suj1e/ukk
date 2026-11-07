# ukk Engineering Standards / 工程规范

> Architecture-first baseline. These rules apply to changes to Desktop, Host, iOS, plugins and all future native adapters.

## Quality gate

A feature is **not complete** when its source merely exists. It must have a tested contract and platform-specific evidence where applicable.

1. Work on a topic branch and open a PR. Do not merge failing checks into `main`.
2. Run `cargo fmt --all -- --check`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` from repository root.
3. Run `cargo fmt --manifest-path apps/desktop/Cargo.toml --all -- --check` and `cargo check --manifest-path apps/desktop/Cargo.toml` for GPUI.
4. Validate macOS, Windows and Linux: GPUI display backends are different, and Windows Named Pipe tests are not replaceable by a Unix CI pass.
5. After every schema/protocol change, add a fixture, migration test, or transport contract test, and update docs.
6. Treat GitHub Actions run status as authoritative. A source commit or a configured workflow is not proof that tests passed.
7. Keep integration tests safe by default: no real Agent runs, destructive process signals or unintended network requests unless the test explicitly opts in.

## Module boundaries

| Area | Owns | Does not own |
|---|---|---|
| `apps/desktop/src/main.rs` | GPUI startup/window lifecycle only | Widgets, Session state, SQL, hc parsing |
| `apps/desktop/src/workspace/state.rs` | Host state refresh and subscriptions | User actions, direct database writes |
| `apps/desktop/src/workspace/actions.rs` | User-intent workspace commands | Agent process lifetime |
| `apps/desktop/src/workspace/{sidebar,panes,agent_detail,toolbar,render}.rs` | Native UI composition | Network authentication, SQL |
| `crates/host/src/cli.rs` | Developer/operator CLI | Host protocol and OS listeners |
| `crates/host/src/transport.rs` | User-scoped Unix Socket and Named Pipe | Product domain rules |
| `crates/host/src/rpc.rs` | RPC routing / local authorization | Rendering, SQLite internals |
| `crates/host/src/broker.rs` | Runtime event continuity | Persisted Session layouts |
| `crates/domain/src` | Stable product identity and pure pane tree algorithms | I/O or herdr-core process control |
| `crates/store/src/schema.rs` | SQLite open/migrations | Product UI |
| `crates/store/src/records.rs` | Space/Session/AgentRun persistence | Pane rendering |
| `crates/store/src/layouts.rs` | Layout validation and CAS | Agent termination |
| `crates/store/src/snapshot.rs` | Read-only durable state materialization | Backend process management |
| `crates/store/src/events.rs` | Durable S1 event-log cursors, retention and snapshot boundary | Wire framing, Host transport |
| `crates/store/src/trust.rs` | Persisted device identity/grants and CAS rotation/revocation | Authorization decisions, key storage |
| `crates/store/src/idempotency.rs` | Durable idempotency begin/finish and tombstones | Command execution, wire results |

Keep modules **cohesive rather than arbitrarily small**. An entrypoint should ideally stay below ~150 lines and should not become a dumping ground. If a module exceeds ~300 lines, review whether it combines independent responsibilities; split when doing so improves ownership and testability. This is a review heuristic, not a rigid lint that encourages meaningless files.

## Rust code conventions

- Modules named after domain responsibilities, not vague `utils`, `helpers` or `misc`.
- Prefer typed commands/results and explicit error types; avoid `unwrap`/`expect` outside tests and startup invariants.
- Do not introduce global mutable application state; isolate concurrency in Host services.
- No blocking I/O inside GPUI `Render`, input handlers or main UI executor. Local RPC and SQLite work must run on background tasks.
- Use `pub(crate)` / `pub(super)` only where cross-module access is needed; keep internals private by default.
- Keep business logic free from GPUI, platform APIs and persistence by using domain-level functions and adapters.
- Use `Result` and explicit typed errors for user operations. On optimistic CAS conflict, surface a retry/reload affordance; never silently overwrite layout.
- Keep secret keys out of logging and UI errors. Security boundaries are additive: local edit capability never authorizes terminal input or remote desktop.
- Do not infer Agent state from raw terminal text; use herdr-core structured status with an explicit unknown fallback.

## Test pyramid

- **Pure unit:** pane split, close, resize; UUID and Session constraints; mismatched CAS revision; invalid/duplicate identifiers.
- **Persistence:** SQLite reopen, migrations and revision conflicts; archived Session retaining AgentRun mappings.
- **Protocol:** named pipe/Unix socket framing, auth failures, idempotent Session creation, daemon restart and event gap recovery.
- **UI integration:** Session creation → Host → SQLite → refreshed GPUI view; closing Pane does not kill Agent; offline and error state.
- **Real OS acceptance:** GPUI launches, keyboard accessibility, permissions, multi-window, and Host persistence on all desktop OSes.

## PR checklist

- [ ] Focused change: clear module ownership, no unrelated UI or infrastructure rewrite
- [ ] Formatting, tests, Clippy, GPUI checks green
- [ ] New behavior has a failing-before / passing-after test, when feasible
- [ ] No process kill on view close, no unintended mutation permissions
- [ ] Security review for credentials, host IPC, plugins, screen capture or input
- [ ] Documentation/ADR and platform matrix updated if architecture changed
- [ ] Actual build/runtime outcomes recorded; unverified items explicitly noted

## Commands

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

cargo fmt --manifest-path apps/desktop/Cargo.toml --all -- --check
cargo check --manifest-path apps/desktop/Cargo.toml
```

The GPUI application intentionally has a separate Cargo workspace to keep UI platform dependencies out of pure Host/domain CI. It is nevertheless subject to the same quality bar.
