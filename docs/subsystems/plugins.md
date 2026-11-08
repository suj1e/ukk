# Plugin architecture — capability-first, safe-by-default

> Status: **v1 contract implemented** in `crates/plugin-api` and
> `crates/plugin-host`, with a harmless sample plugin under
> `plugins/sample-session-action/`. The **Host RPC and Desktop Session wiring
> for that sample are implemented** in `crates/host/src/plugin_service.rs` and
> `apps/desktop/src/workspace/plugins.rs`. There is still **no OS sandbox**; see
> [Not verified](#not-verified-in-this-pr) before relying on this for a
> privileged plugin.

## Scope

ukk plugins extend native Agent sessions without owning the runtime or the
application lifecycle. The broker is deliberately narrow: a plugin declares
commands, event subscriptions and native UI descriptors; the host owns
installation, enablement, grants, dispatch and audit.

Two execution modes exist:

| Trust level | Runtime | Who can install | Status |
|---|---|---|---|
| `builtin` | Trusted code linked into a ukk release, runs **in-process** | Only code compiled into the release | Implemented (`InProcessPlugin`) |
| `first-party` | Separate helper process with explicit entitlements | Bundled installers | Implemented (`HelperProcess`) |
| `third-party` | Separate helper process, restrictive environment | Local install (no store yet) | Implemented (`HelperProcess`) |
| `sandboxed-wasm` | Wasmtime-style sandbox | — | **Rejected** at validation: no runtime ships |

A subprocess is **not** an OS sandbox. First-party and third-party helpers run
with the host user's OS identity; isolation today means process separation,
cleared environment, bounded framing, deadlines and kill-on-disable.

## Delivered surface

| Crate / path | Responsibility |
|---|---|
| `crates/plugin-api/src/manifest.rs` | `plugin.toml` schema v1, validation, contribution descriptors |
| `crates/plugin-api/src/version.rs` | API version and range negotiation (`>=0.1,<0.2`) |
| `crates/plugin-api/src/capability.rs` | Capability vocabulary, risk classes, capability sets |
| `crates/plugin-api/src/wire.rs` | Helper stdio protocol v1 frames |
| `crates/plugin-host/src/supervisor.rs` | `PluginHost`: install, enable/disable/restart/uninstall, supervision |
| `crates/plugin-host/src/dispatch.rs` | Command authorization, event delivery, contribution snapshot |
| `crates/plugin-host/src/state.rs` | Slots, runtime handles, life-cycle state, lock helpers |
| `crates/plugin-host/src/config.rs` | `HostConfig` tunables and limits |
| `crates/plugin-host/src/error.rs` | `HostError` with stable codes |
| `crates/plugin-host/src/messages.rs` | Invoke requests, outcomes, events, publish report |
| `crates/plugin-host/src/helper.rs` | Child-process spawn, bounded framing, deadlines, kill |
| `crates/plugin-host/src/grant.rs` | Scoped grants, TTL rules, revocation |
| `crates/plugin-host/src/audit.rs` | `AuditSink`, record model and redaction on write |
| `crates/plugin-host/src/redact.rs` | Single redaction helper for untrusted text |
| `crates/plugin-host/src/contributions.rs` | Read-only UI descriptors and command index |
| `crates/plugin-host/src/in_process.rs` | Trusted in-process contribution trait |
| `crates/plugin-host/src/bin/*` | Reference sample helper and a fault-injection helper |
| `crates/host/src/plugin_service.rs` | Explicit registry, one Host-owned `PluginHost`, `plugin.*` RPC and typed errors |
| `crates/host/src/plugin_control.rs` | Separate default-off `plugin.control` token file |
| `crates/host-client/src/lib.rs` | Typed `plugin.snapshot` projection and invoke-time token read |
| `apps/desktop/src/workspace/plugins.rs` | Session action/tab/overlay rendering and explicit-click invocation |
| `plugins/sample-session-action/plugin.toml` | Sample manifest: Session action + tab + overlay |

## Manifest schema v1

```toml
manifest_version = 1                  # exactly 1; anything else is rejected
id = "dev.ukk.sample.session-action"  # reverse-DNS, lowercase, ≥2 components
name = "Sample Session Action"        # 1..=80 chars
version = "0.1.0"                     # numeric major.minor.patch, no pre-release
api = ">=0.1,<0.2"                    # must contain the host API version (0.1.0)
trust = "first-party"                 # builtin | first-party | third-party
entrypoint = "ukk-sample-session-helper"  # bare file name; absent for builtin

[capabilities]
required = []                         # declaration only — never a grant
optional = ["session.observe"]

[[contributions.commands]]
id = "sample.session.ping"
title = "Sample: ping this session"
description = "Optional, ≤240 chars"
scope = "session"                     # session | workspace | global
requires = []                         # must be declared in [capabilities]

[[contributions.views]]
id = "sample.session.tab"
slot = "session.tab"                  # session.action | session.tab | session.overlay
                                      # | session.context-panel | settings.section
title = "Sample"
command = "sample.session.ping"       # optional, must be declared above
platforms = ["desktop"]               # desktop | ios; default ["desktop"]

[contributions.views.overlay]         # only for slot = "session.overlay"
anchor = "top-right"                  # top-left|top-right|bottom-left|bottom-right|center
max_width = 320                       # 120..=1080
max_height = 240                      # 120..=1080

[[contributions.events]]
id = "sample.session-events"
kinds = ["session.updated"]           # workspace.changed | session.updated | agent.status
```

Validation is strict and non-negotiable:

- unknown fields at any level → `invalid_toml`;
- unknown capability, slot, scope, event kind or platform → rejected;
- duplicate capability / command / view / event-subscription ids → rejected;
- `requires` may only name declared capabilities (`undeclared_capability`);
- a view may only bind to a declared command;
- an overlay descriptor is required for `session.overlay` and forbidden elsewhere;
- event subscriptions require `session.observe` to be declared;
- an `entrypoint` is a **bare file name** (no `/`, `\`, `..`) and it is
  **bound to the executable that runs**: installing a helper whose program file
  stem differs from the declared entrypoint fails with `invalid_helper`;
- `sandboxed-wasm` is parsed but rejected with an explicit reason.

Limits (see `ukk_plugin_api::limits`): 32 commands, 32 views, 16 event
subscriptions, 32 capability entries per list, 64-char ids, 80-char titles,
64 KiB helper frames, 120–1080 px overlays.

Validation produces `ValidatedManifest`; that is the **only** type the
supervisor accepts, so a partially validated manifest cannot be installed.

### API compatibility

The plugin API version is negotiated independently of the package version.
`>=0.1,<0.2` accepts host `0.1.0`; `>=0.2,<0.3`, `>0.1.0` and malformed ranges
are rejected before any code runs. Pre-1.0, treat the API as unstable: a plugin
built against a newer range must be refused, not best-effort loaded.

## Capabilities and grants

Vocabulary (closed set; unknown strings are rejected):

`workspace.read`, `session.observe`, `agent.send`, `agent.approve`,
`filesystem.read`, `filesystem.write`, `git.write`, `screen.view`,
`screen.control`, `screen.audio`, `notifications.send`,
`network.connect(<domain>)`.

`network.connect` requires a concrete domain (or one `*.` wildcard); a bare
`network.connect` is invalid, so a grant cannot silently mean "the internet".

Risk classes drive grant rules:

| Class | Capabilities | Rules |
|---|---|---|
| Observe | `workspace.read`, `session.observe` | May be long-lived |
| Mutate | `agent.send`, `filesystem.*`, `git.write`, `notifications.send`, `network.connect` | Explicit grant required; TTL optional, ≤24 h when supplied |
| Sensitive | `agent.approve`, `screen.view`, `screen.control`, `screen.audio` | **TTL mandatory**, ≤1 h lease |

Grant semantics:

- grants are keyed by `(plugin, capability, scope)` with scope
  `host` / `space:<id>` / `session:<id>`;
- a `host` grant covers every scope; a `session` grant covers only that session;
  `space` scopes are stored but **do not inherit** down to sessions in v1
  (session→space mapping is intentionally absent rather than guessed);
- installing or enabling a plugin creates **zero** grants; the store type has no
  "default allow";
- a grant can only be issued for a capability the manifest declared;
- revocation is immediate and per-capability; disabling a plugin is **not**
  revocation; uninstalling revokes everything;
- grants live in `InMemoryGrantStore` and do not survive a host restart
  (fail-closed default until durable storage lands).

## Lifecycle

```text
install (disabled) ──enable──> starting ──hello ok──> ready
                                  │                    │
                                  └──> failed <──crash/timeout/protocol
ready ──disable──> disabled ──enable──> ready        (no host restart)
failed ──restart──> disabled ──enable──> ready
any ──uninstall──> removed (+ helper killed, grants revoked, command ids freed)
```

- `enable` spawns the helper (or binds the in-process implementation), performs
  the `plugin.hello` handshake, and validates the reported contribution ids
  against the manifest. Any extra id, wrong protocol or wrong plugin identity
  fails the plugin with `protocol_violation` and kills the process.
- `disable` sends `plugin.shutdown`, waits the grace period, then kills. The
  host process is untouched, so enable/disable is repeatable without restart.
- A plugin that misses a request deadline is **closed, killed and reaped
  immediately**, and its runtime handle is taken out of the supervisor before
  the state becomes `failed`. Later `disable`, `restart`, `uninstall` and host
  teardown therefore cannot orphan it or spawn a second copy. Tested against the
  OS (`kill -0` / `tasklist`), not against the supervisor's own bookkeeping.
- There is **no automatic restart**. `supervise()` detects unexpected exits and
  moves exactly that plugin to `failed`; recovery is an explicit `restart` or
  `enable` by the user/operator.
- A `failed` plugin contributes nothing to the UI and receives no events.

## Helper protocol v1

One JSON object per line over stdin/stdout. Host → helper:

| Method | Purpose |
|---|---|
| `plugin.hello` | Identity + protocol + claimed contribution ids |
| `plugin.health` | Liveness probe with the request deadline |
| `command.invoke` | Run one declared command (`command`, `session_id`, `args`) |
| `plugin.shutdown` | Graceful stop; host kills after the grace period |

Helper → host: `{"id": <request id>, "ok": true, "result": {...}}` or
`{"id": ..., "ok": false, "error": {"code", "message"}}`. `event.deliver`
notifications carry `event` and **no `id`**; answering one is an unsolicited
response and is treated as a protocol violation.

Enforcement on the host side:

- frames are bounded (`max_frame_bytes`, 64 KiB default); an endless line is cut
  off instead of buffered;
- every request has a deadline (2 s default). On expiry the host closes the
  connection, kills and reaps the child, and the plugin becomes `failed`;
- command arguments are size-checked against the frame budget **before** sending.
  Oversized arguments are a caller error (`invalid_args`) and never fail or kill
  the plugin;
- the environment is cleared except an allowlist (`SYSTEMROOT`, `WINDIR`,
  `TEMP`, `TMP`, `TMPDIR`, `HOME`, `USERPROFILE`, `LANG`, `LC_ALL`) plus
  `UKK_PLUGIN_ID`/`UKK_PLUGIN_PROTOCOL`; stderr is discarded;
- the program must be an absolute path resolved by the installer — no `PATH`
  lookup;
- outbound events go through a bounded per-plugin queue; a slow helper causes
  counted drops, never a blocked publisher;
- the protocol carries no grants, approvals or process control, so a helper
  cannot escalate by asking.

## Command dispatch (authorization order)

`PluginHost::invoke(InvokeRequest)` — the single command entry point:

1. plugin installed? otherwise `unknown_plugin`;
2. command declared by **that** plugin? otherwise `forged_command` (owned by
   another plugin) or `unknown_command` (nobody), both audited;
3. plugin `ready`? otherwise `not_enabled`;
4. session-scoped command carries a session id? otherwise `missing_session`;
5. every `requires` capability has an active grant in a matching scope?
   otherwise `capability_denied`;
6. dispatch to helper/in-process; success is audited as `command_invoked`, a
   plugin-returned error as a failed `command_denied`, a crash/timeout/protocol
   failure as the matching audit kind plus a `failed` transition.

UI adapters never pass a command id straight to a process; they pass a
`(plugin_id, command)` pair to the host, which re-checks ownership every time.
`invoke_view(plugin_id, view_id, session)` resolves a descriptor to its declared
command and uses the same path.

## UI contributions

`PluginHost::session_snapshot(platform)` returns only `ready` plugins:

- `session_actions`, `session_tabs`, `session_overlays`, `context_panels`,
  `settings_sections` — each `ViewDescriptor { plugin_id, view_id, slot, title,
  command, overlay }`;
- `commands` — `CommandDescriptor { plugin_id, command_id, title, scope,
  requires }`, so a menu can render a lock for capabilities the user has not
  granted.

Descriptors are plain data: no GPUI, SwiftUI or process handle crosses the
boundary. Platforms filter views, not commands.

## Audit

Every decision goes through `AuditSink`: install, uninstall, manifest rejection,
duplicate registration, enable, disable, enable failure, command invoked,
command denied, unknown command, forged request, helper crashed, helper timeout,
protocol violation, event dropped, grant granted, grant revoked. Records carry
`at_ms`, `plugin_id`, `kind`, `outcome` and a short detail. Details contain ids,
capability names, scope/session and, for grants, the issuing principal
(`by=<granted_by>`) — **never** command arguments, event payloads, file content
or screen data.

Every detail is passed through `redact()` on construction: control characters
(including newline, TAB, ESC/ANSI) and bidi overrides are escaped, and the text
is capped at 200 characters. Helper-authored strings (wire error code/message,
hello identity, frame parse failures) therefore cannot forge log lines, inject
terminal escapes or grow a record without bound. Serde parse failures are
reported by category/line/column instead of echoing the offending input.

**Host wiring:** the Host installs a bounded in-memory sink (512 records; the
oldest is evicted and counted). It records registry rejections, RPC-level
`plugin.control` denials (new `plugin_control_denied` kind), invalid requests
and every supervisor decision (enable/disable/invoke failure, forged command,
crash, timeout, protocol violation). The sink is **not durable** and there is no
audit RPC yet; `plugin.snapshot` exposes only retained/dropped counts. This does
not make audit persistence a delivered control.

`EventDropped` records are **coalesced**: one record for a plugin's first drop
and one per further 64 drops; the authoritative total stays in
`PluginStatus::dropped_events`. A hostile plugin can overflow its own bounded
queue but cannot drive unbounded audit growth.

`MemoryAuditSink` exists for tests; a durable sink is not wired yet.

## Integration seam for GPUI / Host

`InvokeOutcome` carries `provenance: TrustLevel`, so a renderer can tell
helper-authored output (`first-party`/`third-party`, untrusted text) from
`builtin` in-process output. An overlay declared with `session.overlay` is
bounded in absolute pixels only; the renderer **must** clamp it to the hosting
Session view (no full-view takeover, no chrome occlusion).

The adapter surface is intentionally four calls:

```rust
// once per configured plugin, after the embedding host resolved the absolute
// helper path and checked signature/origin
let install = PluginInstall::for_helper(manifest, HelperCommand::new(path))?;
host.install(install)?;
host.enable(&plugin_id)?;

// render
let snapshot = host.session_snapshot(ClientPlatform::Desktop);

// user intent
host.invoke(InvokeRequest::new(plugin_id, command).in_session(session_id))?;

// host events + supervision (timer or before render)
host.publish_event(HostEvent::new(EventKind::AgentStatus, payload).in_session(session_id));
host.supervise();
```

Grant issuance has exactly one door: `PluginHost::grant` checks the manifest,
mints an `AuthorizedGrant` token and writes the audit record. `GrantStore::upsert`
requires that token (no public constructor), so a settings UI or a future
SQLite store cannot create authority on its own; `grant_store()` exposes only
listing and revocation.

## Host registry and `plugin.control` (delivered)

`crates/host/src/plugin_service.rs` owns exactly one `PluginHost` for the Host
process. It never scans a directory. Plugins load only from one explicit
registry file:

- default `~/.ukk/plugins/registry.json` (Windows:
  `%LOCALAPPDATA%\ukk\plugins\registry.json`);
- `UKK_PLUGIN_REGISTRY` overrides the path for tests and operators.

```json
{
  "version": 1,
  "plugins": [
    {
      "id": "dev.ukk.sample.session-action",
      "manifest": "/absolute/path/plugin.toml",
      "helper": "/absolute/path/ukk-sample-session-helper",
      "enabled": true
    }
  ]
}
```

Loading is strict and fail-closed:

- unknown fields and unknown registry versions are rejected;
- manifest and helper paths must be absolute and must not contain `.`/`..`;
- both must be regular, non-symlink files; the helper must be executable and
  its file stem must match the manifest `entrypoint`; the manifest id must
  match the registry id;
- on Unix neither the registry nor the listed files may be group/other
  writable; the registry, manifest and helper must be owned by the Host euid,
  and their real parent directory must be owned by that user with no
  group/other write bits (a file or helper under `/tmp` is refused);
- duplicate ids, manifest paths, helper paths and command ids are ambiguous
  and refuse the whole registry;
- a bad entry installs **nothing**; the snapshot reports `registry.error`;
- a missing registry means "no plugins", not a scan fallback.

Installation is synchronous but starts no process. Registry-flagged plugins are
enabled after the local endpoint is accepting, on a background thread, so a
hung helper cannot delay Host RPC; failures surface as `failed`.

`plugin.enable`/`plugin.disable` persist the flag by re-reading and
revalidating the registry first, then rewriting it atomically (0600 temporary
file + rename). Concurrent operator additions/edits are preserved; an unsafe
or invalid concurrent edit refuses the write. They never restart the Host;
they only start/stop the helper.

### Capability separation

Mutating plugin RPC requires the separate, default-off `plugin.control`
capability. It is **not** the rotated workspace metadata token and **not** the
agent-control token.

- Default path: next to the Host socket with extension `.plugin-control`
  (`~/.ukk/run/agent-host.plugin-control`), or
  `UKK_PLUGIN_CONTROL_TOKEN_FILE`; Windows uses
  `%LOCALAPPDATA%\ukk\run\agent-host.plugin-control`.
- Must be a regular, non-symlink ASCII file, at least 60 characters, mode 0600
  on Unix. It must be owned by the Host euid and live in a directory owned by
  that user with no group/other write bits, so `/tmp` overrides are refused.
- `ukk-host plugin-control-enable` creates it explicitly and refuses to
  overwrite; `plugin-control-disable` removes it; `plugin-control-status`
  reports enabled/path/problem.
- The Host reloads the file on every mutating request, so provisioning or
  revoking it needs no Host restart.
- Only the distinct `control` field is read. The workspace `auth` field and any
  agent-control value are ignored/rejected.
- `plugin.snapshot` is read-only and requires no capability.

Local-only threat model: this is same-user, same-host defense in depth. It does
not authenticate a remote device and does not stop a process that can already
read the user's profile.

### RPC surface

| Method | Capability | Result |
|---|---|---|
| `plugin.snapshot` | none (capability-free) | registry status, installed plugin states (`disabled`/`starting`/`ready`/`failed`), running helper pid, `session.action`/`session.tab`/`session.overlay` descriptors and command metadata. It starts nothing but does call `supervise()`, which may move a crashed helper to `failed`. No helper output, no token, no context/settings slots. Includes a bounded in-memory audit summary (`retained`/`dropped`, `durable: false`) |
| `plugin.enable` | `plugin.control` | starts the helper in the same Host, returns typed state |
| `plugin.disable` | `plugin.control` | stops the helper in the same Host, returns typed state |
| `plugin.invoke` | `plugin.control` | dispatches one declared command with `session_id`/`args`; returns `{plugin_id, command, provenance, value}` |

Denials are typed JSON-RPC errors: plugin-control disabled/insecure/unauthorized
(`-32061`/`-32063`/`-32062`), invalid params (`-32602`), supervisor denial
(`-32065` with `data.kind` such as `not_enabled`, `capability_denied`,
`unknown_command`) and registry failure (`-32066`). The Host plugin path has no
handle to herdr-core or the runtime API.

### Desktop Session rendering

`apps/desktop/src/workspace/plugins.rs` polls `plugin.snapshot` through
`HostClient` and renders the sample action, tab and bounded overlay inside the
selected Session. Selection is local; nothing invokes on load, refresh or tab
selection. Only an explicit action/run click calls `plugin.invoke`, and the
Desktop reads the `plugin.control` token at that moment, never storing or
logging it. Denied and local errors are rendered as typed states; helper
`value` text is sanitized and bounded before display.

Remaining wiring, deliberately **not** in this PR:

1. Grant issuance UI and settings surface for granting/revoking capabilities;
   the sample declares no required capability and this slice issues no grants.
2. Signature/origin verification, installer/allowlist policy and update
   migration. Discovery is explicit, but an absolute path is not yet
   authenticated.
3. Durable grant storage (SQLite) and audit persistence.
4. Host-event production for `workspace.changed`, `session.updated`,
   `agent.status`, plus in-process event delivery.
5. OS sandbox/entitlements for first-party privileged helpers (Remote Screen),
   plus capture/input implementation and consent UI — none of that exists here.

## Security review notes

Threats considered in this PR and how they are addressed:

| Threat | Mitigation | Test |
|---|---|---|
| Manifest silently grants power | Manifests only declare; grants are a separate store starting empty | `host_grants_nothing_by_default_and_privileged_commands_are_denied` |
| Plugin claims another plugin's action | Global command ownership index; `forged_command` denial + audit | `forged_and_unknown_commands_are_denied` |
| Helper invents extra contributions | `plugin.hello` ids validated as a subset of the manifest; mismatch kills it | `hello_forgery_identity_mismatch_and_handshake_timeout_are_rejected` |
| Helper impersonates another plugin | Identity in `plugin.hello` must equal the manifest id | same test (`bad-identity`) |
| Hung helper blocks the host or survives as an orphan | Request deadline, then close + kill + reap + runtime removal; verified against the OS | `helper_timeout_is_bounded_and_marks_the_plugin_failed` |
| Helper floods the host | Bounded frame reader cuts off at the limit | `oversize_helper_output_is_cut_off` |
| Helper corrupts the protocol | Non-JSON / unsolicited response → `protocol_violation` + kill | `non_json_and_unsolicited_frames_are_protocol_violations` |
| One plugin crash kills the app | Process separation; `supervise` scopes failure to one plugin | `helper_crash_is_contained_and_other_plugins_keep_working` |
| Slow helper blocks event publishing | Bounded queue + counted drops | `event_delivery_needs_a_grant_and_a_bounded_queue` |
| Open-ended screen/approval authority | Sensitive capabilities require a ≤1 h TTL lease | `sensitive_capabilities_require_a_bounded_expiry` |
| Grant leaks across sessions | Scope matching, session isolation | `a_grant_authorizes_only_its_scope_and_revocation_is_immediate` |
| Secrets in logs | Audit details exclude arguments/payloads | `audit_records_never_contain_command_arguments` |
| Log forgery via helper text | `redact()` on every audit detail and helper-authored error; serde errors reduced to category | `helper_authored_text_cannot_forge_audit_lines`, `redact` unit tests |
| Unbounded audit growth from queue overflow | Coalesced `EventDropped` records + counter in status | `event_delivery_needs_a_grant_and_a_bounded_queue` |
| Expired lease still authorizing | Use-time `is_active(now)` check, tested with a 1 ms lease and a pause | `expired_sensitive_leases_stop_authorizing` |
| Caller input fails the plugin | Argument budget checked before dispatch → `invalid_args`; encoding overflow is non-fatal | `oversized_arguments_are_a_caller_error_and_do_not_fail_the_plugin` |
| Manifest describes helper A, install runs helper B | `entrypoint` must equal the executed file stem | `entrypoint_must_match_the_helper_that_runs` |
| Session UI bound to a host-wide command | Session slots must bind `session`-scoped commands | `session_slots_must_bind_session_scoped_commands` |
| `PATH` hijack of the helper | Absolute program required | `trust_and_runtime_must_match` (`invalid_helper`) |
| Credential leakage via environment | Environment cleared except allowlist | code review; no test asserts env contents yet |

Residual risks and honest gaps:

- **No OS sandbox.** A third-party helper has full user-level access. Do not
  describe subprocess isolation as a sandbox.
- **No signature/origin verification.** The embedding host must not install a
  path it has not verified; there is no allowlist implementation yet.
- **No durable grants.** Restart drops grants (fail-closed) and audit records.
- **No orphan cleanup on an abnormal host death.** There is no
  `PR_SET_PDEATHSIG`, process group kill or Windows job object, so a SIGKILL or
  crash of the host itself leaves helpers running with the host user's identity.
  Normal `disable`/`uninstall`/timeout/teardown paths all kill and reap; the
  host-crash path needs an OS mechanism and is a follow-up. Runbook today: check
  `ps`/`tasklist` for `ukk-*-helper` after a host crash.
- **In-process plugins are trusted code.** `builtin` must only be used for code
  compiled into the release; capability checks still apply, but there is no
  memory isolation.
- **Denial-of-service within limits remains possible**: a helper can consume CPU
  or spawn its own children. Process/CPU quotas need OS support.
- Session→Space grant inheritance is intentionally unsupported, so a
  space-scoped grant currently authorizes no session command; this is tested
  (`space_grants_authorize_nothing_and_mutate_ttl_is_bounded`).
- Grant expiry uses wall-clock time so a lease survives suspend/resume. A
  backwards clock step can extend an expired lease; hosts that need strictness
  must revoke on resume.

## Not verified in this PR

- macOS/Linux/Windows **runtime** behaviour of the sample helper beyond the
  local test run in this worktree (the three-OS CI matrix is the authority).
- **Third-party helpers.** Only the first-party sample has run through this
  wiring. No third-party helper has been installed, enabled or invoked, and
  there is no signature/origin check.
- **Real GPUI-window acceptance.** The Desktop code compiles and its unit tests
  pass locally, but a real macOS/Windows/Linux window showing plugin slots has
  not been exercised in this worktree. The launch-smoke workflow remains the
  authority for window evidence.
- Capture/input (no such capability is implemented; `screen.*` is vocabulary
  only), OS sandboxing and entitlements.
- Durable grant/audit persistence and multi-process host operation. The Host
  plugin audit sink is bounded and in-memory only.
- CPU/memory quotas, helper restart budgets and crash-loop policy.
- Signature, notarization, allowlist and update/migration for plugins.

## Reproducing the sample

```bash
cargo test -p ukk-plugin-api            # manifest/version/capability contract
cargo test -p ukk-plugin-host           # lifecycle, dispatch, crash, queue, audit
cargo test -p ukk-host plugin           # registry validation, control, service
cargo test -p ukk-host --test plugin_wiring  # real helper through Host RPC
cargo test -p ukk-host-client           # snapshot parsing and token reader
cargo test --manifest-path apps/desktop/Cargo.toml --bin ukk-desktop
```

`sample_plugin_enable_invoke_disable_without_host_restart` (plugin-host) proves
the broker in isolation. The Host-level
`sample_plugin_enable_invoke_disable_without_host_restart` in
`crates/host/tests/plugin_wiring.rs` starts the actual `ukk-host` binary with an
explicit temp registry, enables/invokes/disables/re-enables the real
`ukk-sample-session-helper`, and asserts the Host process id is unchanged.
`plugin_control_is_default_off_and_never_accepts_other_tokens` proves the
workspace/agent-style tokens are refused, and
`plugin_rpc_does_not_change_runtime_observation` proves plugin work does not
alter the Host runtime snapshot.

To run it by hand:

```bash
# 1. Build the sample helper and the Host.
cargo build -p ukk-host -p ukk-plugin-host
# 2. Write ~/.ukk/plugins/registry.json with absolute manifest/helper paths.
# 3. Provision the separate capability.
target/debug/ukk-host plugin-control-enable
# 4. Start the Host, then use `plugin.snapshot` / `plugin.invoke` over the
#    local socket. The Desktop reads the same snapshot through HostClient.
target/debug/ukk-host serve
```

## Compatibility and store strategy

- API version is negotiated per manifest; hosts refuse ranges that exclude them.
- Manifest schema version is exact (`1`); adding fields is a new schema version.
- Pre-1.0: pin plugin versions, keep an allowlist, and treat third-party
  distribution/marketplace as future work. No unreviewed code download exists.
- Wasm sandboxing is a schema placeholder only and is rejected until a runtime
  is validated.

See [remote screen](remote-screen.md), [security](security.md),
[architecture](../tech-design.md) and [roadmap](../roadmap.md).
