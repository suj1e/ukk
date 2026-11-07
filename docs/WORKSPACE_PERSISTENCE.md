# Workspace Persistence — Spike 04

> 2026-11-07 · Source committed, compile/CI/macOS-Windows-Linux runtime acceptance **not yet verified**. This is the first durable product-level model, not a complete Coding Agent frontend.

## Product behavior

ukk's **Space**, **Session**, **AgentRun** and **Pane** are different things:

- **Space** organizes a project on a Host. It is persisted independently of runtime terminals.
- **Session** is a named work context under one Space. Archived does **not** mean terminated.
- **AgentRun** explicitly binds a Session to an *existing* herdr-core terminal, identified by `core_id + terminal_address`. A binding is metadata, not a process owner; it cannot launch/kill the agent.
- **Pane** is a native UI surface that references an AgentRun, terminal, Files, Diff or Activity. Closing a pane is presentation-only.
- **SessionLayout** is a versioned split tree; any number of panes can reference the same AgentRun.
- The live terminal snapshot still comes from the Host's independent herdr-core broker; a stored binding is not proof that the process remains online.

```mermaid
flowchart TB
  subgraph Desktop["GPUI Desktop · macOS / Windows / Linux"]
    UI["Spaces / Sessions sidebar"]
    PANES["Persisted native Pane tree<br/>horizontal / vertical splits"]
    UI --> PANES
  end
  subgraph Host["ukk Host · shared code"]
    READ["Read-only workspace.snapshot API"]
    CLI["Local explicit CLI edits"]
    STORE["ukk-store<br/>SQLite schema v3 + CAS revisions"]
    READ --> STORE
    CLI --> STORE
    B["Runtime event broker"] --> HC["herdr-core · owns processes"]
  end
  UI <-->|"Local IPC"| READ
  PANES -. "AgentRun core_id / terminal_address" .-> B
  STORE -. "metadata only; never kill processes" .-> HC
```

## Files

- `crates/domain/` — serde-friendly typed entities; recursive validated `PaneNode::Split` and `PaneNode::Leaf`; immutable split/close helpers; no GPUI dependency.
- `crates/store/` — SQLite with bundled engine, schema version 3, FK constraints, WAL, atomic Session + default layout creation, optimistic `revision` update and tests. `crates/store/src/backup.rs` owns consistent snapshots, guarded restore and read-only recovery diagnostics.
- `crates/store/src/events.rs` — durable S1 event log: transactional cursor assignment, range read, retention floor and snapshot boundary.
- `crates/store/src/trust.rs` — device identity, scoped grants, rotation/revocation CAS and `TrustRegistry` hydration.
- `crates/store/src/idempotency.rs` — atomic idempotency begin/finish, in-flight retention, completion tombstones and capacity refusal.
- `crates/host/` — read-only `workspace.snapshot` JSON-RPC for GPUI; local CLI creation, archiving, agent binding and split/close.
- `crates/host-client/` — shared typed `DomainSnapshot` fetch over Unix sockets and Windows Named Pipes.
- `apps/desktop/` — Space / Session sidebar and recursive split-pane display driven by persisted layout, not fake Agent dialogues.

## Database

Default local location:

| OS | Default path |
|---|---|
| macOS / Linux | `~/.ukk/data/workspace.sqlite3` |
| Windows | `%LOCALAPPDATA%\ukk\workspace.sqlite3` |

`UKK_DB_PATH` overrides the database path for development/testing. This configuration belongs to **Host**; Desktop never writes SQLite directly.

- The default Unix data directory is mode 0700 and SQLite file is created with mode 0600.
- WAL allows concurrent CLI writers and Host snapshot readers.
- Foreign keys enforce associations and unique `(core_id, terminal_address)` avoids duplicate terminal bindings.
- `PRAGMA user_version=3` migration prevents silently loading newer schema versions; v1 → v2 → v3 upgrades run under one `IMMEDIATE` transaction.
- Layout edits use `expected_revision`; stale updates return a Conflict instead of overwriting another view.
- A Session created by CLI starts with one Activity pane, rather than a pretend chat transcript.
- The storage layer contains project metadata, not terminal output, secrets, video frames, approvals or chat transcripts.

## S1 protocol state (schema v3)

Schema v3 adds the durable state the S1 remote protocol needs after a Host
restart. `crates/store` depends on `ukk-protocol` for the cursor, trust and
idempotency types and decisions; it persists and reloads them without adding a
transport or a permission surface. Existing Space / Session / AgentRun / layout
tables and their CAS semantics are unchanged.

| Table | Holds |
|---|---|
| `event_streams` | one row per stream: protocol generation, durable `head`, retention `floor` |
| `event_log` | ordered `(stream, cursor)` rows with event id, generation and payload JSON |
| `devices` | device id, public-key fingerprint, identity generation, revoked status |
| `device_grants` | scope, grant generation and absolute expiry per device |
| `idempotency_records` | key, command fingerprint, plane, principal, state, result and timestamps |

Contract:

- **Append is transactional.** `Store::append_event` opens an `IMMEDIATE`
  transaction, validates the typed event against `Limits`, assigns `head + 1`,
  inserts the event and advances the head in one commit. Duplicate event ids,
  oversized payloads, a mismatched stored generation and cursor exhaustion are
  refused before the head moves.
- **Retention is monotonic.** `Store::prune_events_through` deletes covered
  events and advances the floor; moving the floor backwards is a typed error.
  Append also prunes to `Limits::max_retained_events`.
- **Reads use protocol outcomes.** `Store::read_events` returns
  `Resume::Events`, `Resume::SnapshotRequired { at_cursor: head }` below the
  floor, or `CursorBeyondHead`; it never resets a cursor. Corrupt gaps and
  generation mismatches are storage errors.
- **Snapshot boundary.** `Store::snapshot_at_head` binds a domain-owned body to
  the current durable head and enforces the snapshot size bound.
- **Trust is CAS-guarded.** `register_device` starts at generation 1.
  `rotate_device_key` and `revoke_device` take the expected generation, reject a
  stale writer, and clear grants atomically. `load_trust_registry` hydrates the
  protocol `TrustRegistry`; authorization is not reimplemented in the store.
- **Idempotency is atomic.** `begin_idempotent_command` performs purge, lookup,
  capacity check and insert in one `IMMEDIATE` transaction. Exactly one
  concurrent retry receives `Execute`. In-flight records never expire; completed
  records become tombstones after retention and still consume capacity; a full
  store refuses new keys instead of evicting.
- **No secrets.** Device rows store only the public-key fingerprint and grant
  policy. Private keys, pairing secrets and session keys are not written to
  SQLite.

**NOT VERIFIED:** Host RPC/transport wiring, relay/WAN delivery, OS keychain
storage, real phone/relay reconnect, and multi-process contention beyond the
SQLite busy timeout are not exercised by this adapter. The store contract is
covered by reopen, cursor-regression, retention, CAS, concurrent-begin and
migration tests; real transport integration remains a separate milestone.

## Backup, restore and recovery

The Host metadata database is small, but losing it loses Spaces, Sessions,
AgentRun bindings and pane layouts. `ukk-host` exposes three explicit,
local-only commands. None is an RPC, and none prints workspace rows.

```bash
# Consistent snapshot while the Host is running (WAL contents included)
cargo run -p ukk-host -- backup ~/Backups/ukk-workspace-20261108.sqlite3

# Replace the database after stopping the Host. The current file is backed up
# automatically next to it as workspace.sqlite3.pre-restore-*.sqlite3.
cargo run -p ukk-host -- restore ~/Backups/ukk-workspace-20261108.sqlite3

# Read-only diagnosis: path, schema, integrity, WAL/journal state and guidance.
cargo run -p ukk-host -- db-doctor
```

### Backup semantics

- Uses SQLite's **online backup API**. The source connection holds a read
  transaction, so the snapshot is consistent while the Host continues writing
  in WAL mode.
- Writes to a private temporary file (`0600` on Unix) in the destination
  directory, verifies `PRAGMA integrity_check`, the supported `user_version`
  (1–3) and the required tables, forces a single-file `DELETE` journal,
  flushes it (`fsync` on Unix; `MOVEFILE_WRITE_THROUGH` on Windows), and only
  then atomically renames it over the destination. A failure never leaves a
  partial destination.
- Refuses an existing destination unless `--force` is passed, and always
  refuses symlinks, directories, devices and the source file itself.
- The snapshot is metadata only (no terminal output, chat transcripts or
  secrets), but it still contains project paths and Session titles: keep it
  private. Backup files are **not encrypted**; OS file permissions are the
  boundary.
- A backup of a v1 or v2 database is accepted; the next normal Host open
  migrates it to v3.

### Restore semantics

1. Validate the source read-only: SQLite readable, `integrity_check=ok`,
   schema version 1–3 and the required ukk tables present. Corrupt, foreign and
   newer-schema files are rejected before anything is changed.
2. Materialize a verified, single-file replacement in the database directory.
3. If the current database exists, copy it automatically to
   `<name>.pre-restore-<timestamp>.sqlite3`. A consistent snapshot is used when
   the file is readable; a corrupt/foreign file is preserved byte-for-byte,
   including any `-wal`/`-shm`/`-journal` sidecars.
4. Refuse while another Host owns the database when detectable. On Unix the
   restore holds the same `flock` lifecycle lock as `serve`, so a Host cannot
   start during the replacement; on Windows a live Named Pipe is probed. An
   additional `BEGIN EXCLUSIVE` check rejects an active non-Host writer.
5. Atomically rename the replacement over the database, remove the old
   database's sidecars, and re-run integrity/schema verification.
6. If verification fails after the rename, restore the automatic safety copy
   and report the rollback. Failures before the rename leave the original file
   untouched.

Restore does not migrate the schema itself; the next normal Host open performs
the v1/v2→v3 migration. The Host should be stopped before restore because its
in-memory connection would otherwise keep using the replaced inode.

### `db-doctor`

Reports only metadata: path, Unix mode, size, schema version, integrity
result, journal mode, sidecar sizes and an open error if any. It never runs a
row-returning query. Guidance distinguishes a missing database, a corrupt
file, an unsupported newer schema, an uncheckpointed WAL and a leftover
rollback journal.

### Current limitations

- Heavy concurrent-writer behavior during a live backup is covered only by a
  single open-connection test; sustained multi-writer stress is **not
  verified**.
- Restore expects a self-contained SQLite file, such as one produced by
  `backup`. A crashed WAL database whose `-shm` file is missing must be opened
  once by the Host (which recovers it) or copied together with its sidecars.
- Windows live-Host detection is a Named Pipe probe; there is no cross-process
  lifecycle lock equivalent to the Unix `flock`, so the Host could theoretically
  start in the small window after the probe. The SQLite exclusive check is the
  second line of defense.

## Local developer walkthrough

First, start herdr-core and the ukk Host when possible. You can create metadata before herdr-core is installed; the DB is independent of runtime.

```bash
# Build and run tests from repo root
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Create a Space for your own actual project root
cargo run -p ukk-host -- space-create ukk /path/to/ukk

# The output includes an actual Space ID. Use it below:
cargo run -p ukk-host -- session-create <space-id> "Implement sync protocol"

# Inspect Session IDs, initial pane ID and layout revision
cargo run -p ukk-host -- workspace-dump

# Split an existing leaf to add a Diff view (prints new pane IDs)
cargo run -p ukk-host -- pane-split <session-id> <pane-id> horizontal diff

# Remove a leaf: this only collapses its split; agents keep running
cargo run -p ukk-host -- pane-close <session-id> <pane-id-to-close>

# Optional: create a manual binding ONLY after verifying actual core ID and terminal address
cargo run -p ukk-host -- agent-bind <session-id> <actual-core-id> <actual-address> codex

# Metadata archiving never sends a kill to herdr-core
cargo run -p ukk-host -- session-archive <session-id>

# Terminal 1: Host, which owns the broker and reads SQLite
cargo run -p ukk-host -- serve

# Terminal 2: query only; no mutation over the local socket
cargo run -p ukk-host -- request workspace.snapshot

# Terminal 3: native GPUI prototype (Windows / macOS / Linux build verification still pending)
cargo run --manifest-path apps/desktop/Cargo.toml
```

The CLI uses quoted arguments. The example angle-bracket placeholders must be replaced with real IDs before running. `pane-split` currently starts a 50/50 split; layout model stores ratios but interactive drag handles are **not** wired into GPUI yet.

## Security, consistency and current limitations

- Local socket/Named Pipe `workspace.snapshot` is **read-only**; no new write RPCs or remote privileges were exposed.
- Mutations require explicit local CLI invocation. Native UI creation/rename/archive actions will follow after local command authorization is designed.
- Metadata changes made by CLI currently appear in Desktop on manual **Refresh**. The live herdr-core Agent stream separately auto-updates; future metadata change events should remove this manual step.
- Layout validation rejects duplicate IDs, excessive nesting, more than 16 leaves, NaN/invalid ratios, unknown pane targets or AgentRun references from another Session.
- A layout tree is not yet a draggable multi-pane implementation: GPUI renders the stored split structure but ratio-resizing, drag/reorder, tab management and per-device layout preferences are still planned.
- Dynamic herdr-core terminals are intentionally not auto-assigned to a random Session. Mapping requires user intent and actual core identity; orphan terminals stay visible in the runtime list.
- Product-level Sessions survive UI and Host restarts. **Live Agent execution** still depends on herdr-core's independent persistence and its compatibility with the current runtime.
- A SQLite file cannot protect against another malicious process running as the same OS user. Device trust persistence exists, but pairing cryptography, OS key storage, plugin sandbox and remote transport authorization remain separate security milestones.
- SQLite migrations are covered by automated tests; CI and platform-specific file permissions still need actual runtime testing before release.

## Tests added

- Domain: split/close roundtrip, preserve Agent identity, reject last-pane removal, reject invalid ratios, duplicate IDs, JSON format stability.
- Store: DB reopen persistence, optimistic revision conflict, cross-Session binding rejection, archived Session not removing AgentRun.
- Store (S1, schema v3): event-log reopen and continued cursors, cursor regression, retention floor/snapshot boundary, duplicate event id, generation mismatch; device grants across reopen/expiry, rotation/revocation CAS invalidation; idempotency concurrent begin, tombstone expiry, capacity refusal, plane/principal/fingerprint conflicts; v1→v3 and v2→v3 migration with workspace data preserved.
- Store backup/restore: backup while the source connection is open, integrity/schema verification, corrupt/foreign/empty/newer-schema rejection, overwrite and symlink refusal, `0600` permissions, automatic safety backup, rollback after failed post-install verification, active-writer refusal, v1/v2 restore + Host migration, missing-database restore, and reopen after restore. All tests use temporary directories.
- Host integration: mock hc live event stream + local socket `workspace.snapshot` from persisted SQLite.
- Windows: Windows Named Pipe test additionally reads a persisted Space and Session from Host.
- Three-OS Rust CI, GPUI build workflow already configured; success is **not** confirmed.

## Next engineering slice

1. Get cross-platform CI green and collect *real* Mac/Windows/Linux logs/screenshots.
2. Add authenticated local mutation commands for Desktop and metadata revision notifications.
3. Build true GPUI resizable splitter, pane focus, drag/reorder and tabs with saved layouts.
4. Reconcile explicit AgentRun bindings with hc identity and structured Agent provider events.
5. Then native chat, Diff, Files, Git, approvals and plugin contribution slots.

Related: [Domain Model](DOMAIN_MODEL.md), [Architecture](ARCHITECTURE.md), [Roadmap](roadmap.md).
