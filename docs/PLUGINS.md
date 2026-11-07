# Plugin architecture — capability-first, safe-by-default

> Status: **v1 contract implemented** in `crates/plugin-api` and
> `crates/plugin-host`, with a harmless sample plugin under
> `plugins/sample-session-action/`. There is **no GPUI or Host RPC wiring yet**
> and no OS sandbox; see [Not verified](#not-verified-in-this-pr) before relying
> on this for a privileged plugin.

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
| `crates/plugin-host/src/audit.rs` | `AuditSink` and redaction rules |
| `crates/plugin-host/src/contributions.rs` | Read-only UI descriptors and command index |
| `crates/plugin-host/src/in_process.rs` | Trusted in-process contribution trait |
| `crates/plugin-host/src/bin/*` | Reference sample helper and a fault-injection helper |
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
- an `entrypoint` is a **bare file name** (no `/`, `\`, `..`): the installer, not
  the manifest, decides which absolute executable runs;
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
- every request has a deadline (2 s default); a hung helper is killed and the
  plugin marked failed;
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
capability names and reasons only — **never** command arguments, event payloads,
file content or screen data. `MemoryAuditSink` exists for tests; a durable sink
is not wired yet.

## Integration seam for GPUI / Host

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

Remaining wiring, deliberately **not** in this PR:

1. Host RPC method(s) exposing snapshot/invoke/grants — `crates/host/src/rpc.rs`
   stays untouched; local authorization for mutation methods must be designed
   first (see H1).
2. GPUI rendering of `ViewDescriptor`/`OverlayDescriptor` and a settings surface
   for granting/revoking capabilities.
3. Plugin discovery, signature/origin verification, allowlist and installer
   policy; today the embedding host must supply the absolute helper path.
4. Durable grant storage (SQLite) and audit persistence.
5. Host-event production: which Host subsystem emits `workspace.changed`,
   `session.updated`, `agent.status`, and how in-process plugins receive events.
6. OS sandbox/entitlements for first-party privileged helpers (Remote Screen),
   plus capture/input implementation and consent UI — none of that exists here.

## Security review notes

Threats considered in this PR and how they are addressed:

| Threat | Mitigation | Test |
|---|---|---|
| Manifest silently grants power | Manifests only declare; grants are a separate store starting empty | `host_grants_nothing_by_default_and_privileged_commands_are_denied` |
| Plugin claims another plugin's action | Global command ownership index; `forged_command` denial + audit | `forged_and_unknown_commands_are_denied` |
| Helper invents extra contributions | `plugin.hello` ids validated as a subset of the manifest; mismatch kills it | `hello_forgery_identity_mismatch_and_handshake_timeout_are_rejected` |
| Helper impersonates another plugin | Identity in `plugin.hello` must equal the manifest id | same test (`bad-identity`) |
| Hung helper blocks the host | Request deadline, then kill + `failed` transition | `helper_timeout_is_bounded_and_marks_the_plugin_failed` |
| Helper floods the host | Bounded frame reader cuts off at the limit | `oversize_helper_output_is_cut_off` |
| Helper corrupts the protocol | Non-JSON / unsolicited response → `protocol_violation` + kill | `non_json_and_unsolicited_frames_are_protocol_violations` |
| One plugin crash kills the app | Process separation; `supervise` scopes failure to one plugin | `helper_crash_is_contained_and_other_plugins_keep_working` |
| Slow helper blocks event publishing | Bounded queue + counted drops | `event_delivery_needs_a_grant_and_a_bounded_queue` |
| Open-ended screen/approval authority | Sensitive capabilities require a ≤1 h TTL lease | `sensitive_capabilities_require_a_bounded_expiry` |
| Grant leaks across sessions | Scope matching, session isolation | `a_grant_authorizes_only_its_scope_and_revocation_is_immediate` |
| Secrets in logs | Audit details exclude arguments/payloads | `audit_records_never_contain_command_arguments` |
| `PATH` hijack of the helper | Absolute program required | `trust_and_runtime_must_match` (`invalid_helper`) |
| Credential leakage via environment | Environment cleared except allowlist | code review; no test asserts env contents yet |

Residual risks and honest gaps:

- **No OS sandbox.** A third-party helper has full user-level access. Do not
  describe subprocess isolation as a sandbox.
- **No signature/origin verification.** The embedding host must not install a
  path it has not verified; there is no allowlist implementation yet.
- **No durable grants.** Restart drops grants (fail-closed) and audit records.
- **In-process plugins are trusted code.** `builtin` must only be used for code
  compiled into the release; capability checks still apply, but there is no
  memory isolation.
- **Denial-of-service within limits remains possible**: a helper can consume CPU
  or spawn its own children. Process/CPU quotas need OS support.
- Session→Space grant inheritance is intentionally unsupported, so a
  space-scoped grant currently authorizes no session command.

## Not verified in this PR

- macOS/Linux/Windows **runtime** behaviour of the sample helper beyond the
  local test run in this worktree (CI matrix is the authority).
- Any GPUI rendering, Host RPC path, mobile/iOS consumption.
- OS sandboxing, entitlements, screen capture/input (no such capability is
  implemented; `screen.*` is vocabulary only).
- Durable grant/audit persistence and multi-process host operation.
- CPU/memory quotas, helper restart budgets and crash-loop policy.
- Signature, notarization, allowlist and update/migration for plugins.

## Reproducing the sample

```bash
cargo test -p ukk-plugin-api            # manifest/version/capability contract
cargo test -p ukk-plugin-host           # lifecycle, dispatch, crash, queue, audit
```

The sample manifest is validated from its shipped file
(`plugins/sample-session-action/plugin.toml`), and
`sample_plugin_enable_disable_without_host_restart` installs it against the real
`ukk-sample-session-helper` binary, enables it, dispatches
`sample.session.ping`, disables it and enables it again inside one host instance.

## Compatibility and store strategy

- API version is negotiated per manifest; hosts refuse ranges that exclude them.
- Manifest schema version is exact (`1`); adding fields is a new schema version.
- Pre-1.0: pin plugin versions, keep an allowlist, and treat third-party
  distribution/marketplace as future work. No unreviewed code download exists.
- Wasm sandboxing is a schema placeholder only and is rejected until a runtime
  is validated.

See [remote screen](REMOTE_SCREEN.md), [security](SECURITY.md),
[architecture](ARCHITECTURE.md) and [roadmap](roadmap.md).
