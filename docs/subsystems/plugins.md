# Plugin architecture: capability-first, safe by default

> Contract for ukk's extension boundary in the Go implementation. It defines the
> manifest, capability and grant vocabulary, the helper framing and the
> supervisor rules the bridge must enforce. It is not runtime evidence: no Go
> plugin supervisor is wired into the bridge yet, and what is implemented and
> verified is recorded in [status](../status.md). Where this page and the code
> disagree, the code and the recorded evidence win, and this page must be
> corrected.

## Scope

Plugins extend native Agent Sessions without owning the runtime, the
application lifecycle or the bridge's privileged process. The broker is narrow
on purpose: a plugin declares commands, event subscriptions and native UI
descriptors; the bridge owns installation, enablement, grants, dispatch and
audit.

| Trust level | Runtime | Install path | Placement |
| --- | --- | --- | --- |
| `builtin` | Trusted code linked into the release, runs in-process | Only code compiled into a ukk build | Reviewed first-party |
| `first-party` | Helper subprocess shipped with the release | Bundled installer | Out-of-process |
| `third-party` | Helper subprocess, restrictive environment | Local install, explicit registry entry | Out-of-process |

A subprocess is **not an OS sandbox**. First- and third-party helpers run with
the bridge user's OS identity; isolation means process separation, a cleared
environment, bounded framing, deadlines, crash containment and kill-on-disable.
Sandboxing where an OS provides it is separate work; no sandbox is claimed
here. A Wasm runtime is not shipped and such a manifest is rejected.

## Manifest schema v1 (JSON)

The manifest is JSON, not TOML. One file, one object, decoded strictly.

```json
{
  "manifest_version": 1,
  "id": "dev.ukk.sample.session-action",
  "name": "Sample Session Action",
  "version": "0.1.0",
  "api": ">=0.1,<0.2",
  "trust": "third-party",
  "entrypoint": "ukk-sample-session-helper",
  "capabilities": {
    "required": [],
    "optional": ["session.observe"]
  },
  "contributions": {
    "commands": [
      {
        "id": "sample.session.ping",
        "title": "Sample: ping this session",
        "description": "Optional, at most 240 characters",
        "scope": "session",
        "requires": []
      }
    ],
    "views": [
      {
        "id": "sample.session.tab",
        "slot": "session.tab",
        "title": "Sample",
        "command": "sample.session.ping",
        "platforms": ["desktop"],
        "overlay": null
      }
    ],
    "events": [
      { "id": "sample.session-events", "kinds": ["session.updated"] }
    ]
  }
}
```

### Strict decoding

- Exactly one JSON object; trailing data, wrong types and unknown fields at
  **any** depth are rejected (`invalid_manifest`).
- `manifest_version` must be exactly `1`.
- Unknown trust level, capability, slot, command scope, event kind or platform
  is rejected. There is no "ignore what you do not understand" mode.
- Duplicate capability names, command ids, view ids or event subscription ids
  are rejected.
- `requires` may only name capabilities declared in `required` or `optional`
  (`undeclared_capability`).
- A view `command` must reference a command declared in the same manifest.
- An `overlay` descriptor is required for `session.overlay` and forbidden for
  every other slot.
- Event subscriptions require `session.observe` to be declared.
- `entrypoint` is a bare file name with no `/`, `\` or `..`; for a
  non-`builtin` plugin it must match the installed helper's file stem
  (`invalid_entrypoint`). `builtin` plugins have no entrypoint.
- The `api` range must parse, must include the bridge API version, and must be
  compatible; a plugin built for a newer range is refused, never best-effort
  loaded.

A decoded manifest becomes an immutable validated value; the supervisor accepts
nothing else, so a partially validated manifest cannot be installed.

### Identifiers and limits

| Item | Limit |
| --- | --- |
| Plugin id | reverse-DNS, lowercase, at least two components, ≤64 characters |
| Command / view / event ids | ≤64 characters, `[a-z0-9._-]`, non-empty |
| Plugin name | 1–80 characters |
| Command description | ≤240 characters |
| Commands | ≤32 |
| Views | ≤32 |
| Event subscriptions | ≤16 |
| Capability entries per list | ≤32 |
| Helper frame | ≤64 KiB of JSON after the 4-byte length prefix |
| Overlay size | 120–1080 px per dimension |
| API version | `major.minor.patch` string; range operators `>=`, `<` only |

## Capabilities

The vocabulary is closed; unknown strings are rejected, not ignored.

`workspace.read`, `session.observe`, `agent.send`, `agent.approve`,
`filesystem.read`, `filesystem.write`, `git.write`, `screen.view`,
`screen.control`, `screen.audio`, `notifications.send`,
`network.connect(<domain>)`.

`network.connect` requires a concrete domain or one `*.` wildcard domain. A
bare `network.connect` is invalid, so a grant can never silently mean "the
internet".

Risk classes drive grant rules:

| Class | Capabilities | Grant rule |
| --- | --- | --- |
| Observe | `workspace.read`, `session.observe` | May be long-lived |
| Mutate | `agent.send`, `filesystem.read`, `filesystem.write`, `git.write`, `notifications.send`, `network.connect(...)` | Explicit grant required; TTL optional, at most 24 h when supplied |
| Sensitive | `agent.approve`, `screen.view`, `screen.control`, `screen.audio` | TTL mandatory, at most 1 h; screen control also needs a lease |

Declaring a capability is not a grant. Installing or enabling a plugin creates
**zero** grants, and no code path defaults to allow.

## Grants

- Grants are keyed by `(plugin, capability, subject)` where subject is `host`,
  `space:<id>` or `session:<id>`.
- A `host` grant covers every subject. A `session` grant covers that Session
  only. `space` subjects are stored but do not inherit down to Sessions in v1;
  no guessing of the mapping.
- A grant can only be issued for a capability the manifest declared.
- Issue, expiry and revocation are bridge decisions. The Desktop renders them;
  it does not mint authority.
- Revocation is immediate and per capability. Disabling a plugin is **not**
  revocation. Uninstalling revokes everything for that plugin.
- Grants must be durable across a bridge restart with fail-closed defaults; an
  unreadable grant store yields no authority, never "everything granted".
- Remote devices may invoke plugins only with the bridge-level
  `plugin.control` scope; plugin management (install, enable, disable, grant,
  revoke) is local-only.

Grant issuance has one door in the supervisor: it validates the manifest,
mints a typed authorized grant value and writes an audit record. The grant
store accepts only that value, so a settings surface cannot create authority by
calling the store directly.

## Subprocess protocol v1

### Framing

Every frame is:

```text
u32 big-endian length || length bytes of one JSON object
```

- The length prefix excludes itself and must be 1..65536. Zero or oversized
  lengths are a protocol violation.
- The frame body must be exactly one JSON object with no trailing bytes.
- Read and write are deadline-bounded. A partial frame that exceeds the
  deadline is a protocol violation; the helper is killed and reaped.
- Request and response carry the same `id`. `event.deliver` notifications carry
  no `id`; answering one is an unsolicited response and therefore a protocol
  violation.

Host → helper:

| Method | Purpose |
| --- | --- |
| `plugin.hello` | Plugin identity, protocol version and claimed contribution ids |
| `plugin.health` | Liveness probe under the request deadline |
| `command.invoke` | Run one declared command with `command`, `session_id`, `args` |
| `plugin.shutdown` | Graceful stop; the host kills after the grace period |

Helper → host:

| Frame | Meaning |
| --- | --- |
| `{"id":"…","ok":true,"result":{…}}` | Successful response |
| `{"id":"…","ok":false,"error":{"code":"…","message":"…"}}` | Typed failure |
| `{"event":{"kind":"…","payload":{…}}}` | Delivered `event.deliver` notification; no `id` |

`hello` is mandatory: a helper that fails it, reports a different plugin id or
claims contribution ids that are not in the manifest is failed with
`protocol_violation` and killed.

### Spawn and limits

- The helper path is absolute and resolved by the installer; no `PATH` lookup.
- The file must be a regular executable owned by the bridge user; on Unix it
  and its parent directories must have no group/other write permission.
  Symlinks are refused.
- The environment is cleared except `SYSTEMROOT`, `WINDIR`, `TEMP`, `TMP`,
  `TMPDIR`, `HOME`, `USERPROFILE`, `LANG`, `LC_ALL`, plus
  `UKK_PLUGIN_ID` and `UKK_PLUGIN_PROTOCOL`.
- stderr is captured or discarded up to a fixed bound and never forwarded
  verbatim to the UI.
- Every request has a deadline (2 s default, long enough only for a declared
  command). Expiry closes the pipes, kills and reaps the helper, and moves the
  plugin to `failed`.
- Command arguments are size-checked against the frame budget before sending.
  Oversized arguments are a caller error (`invalid_args`) and never kill the
  plugin.
- Outbound events use a bounded per-plugin queue. A slow helper causes counted
  drops, never a blocked publisher.
- Disable, uninstall, bridge shutdown and an unresponsive helper all
  kill-on-disable: the supervisor removes the runtime handle before reporting
  state, so a later disable/restart cannot orphan or duplicate the process.
  Descendant processes are not claimed contained without an OS sandbox.

## Lifecycle

```text
install (disabled) ──enable──> starting ──hello ok──> ready
                                  └──> failed (crash, timeout, protocol error)
ready ──disable──> disabled ──enable──> ready
failed ──restart──> disabled ──enable──> ready
any ──uninstall──> removed (helper killed, grants revoked, ids freed)
```

- `enable` spawns the helper or binds the in-process implementation, performs
  the `hello` handshake and validates claimed ids. It creates no grants.
- `disable` sends `plugin.shutdown`, waits the grace period, then kills. The
  bridge process is untouched, so enable/disable is repeatable without restart.
- There is no automatic restart. An unexpected exit moves exactly that plugin to
  `failed`; recovery is an explicit user or operator action.
- A `failed` plugin contributes no views, receives no events and can be invoked
  by no client.

## Dispatch authorization order

One entry point handles every invocation:

1. Is the plugin installed? Otherwise `unknown_plugin`.
2. Is the command declared by **that** plugin? Otherwise `forged_command` (owned
   by another plugin) or `unknown_command` (nobody); both are audited.
3. Is the plugin `ready`? Otherwise `not_enabled`.
4. Does a Session-scoped command carry a Session id? Otherwise
   `missing_session`.
5. Does every `requires` capability have a live grant in a matching subject?
   Otherwise `capability_denied`.
6. Dispatch. Success is audited as `command_invoked`; a helper-reported error is
   a failed `command_denied`; crash, timeout or protocol failure is audited as
   that kind plus a `failed` transition.

UI adapters pass `(plugin_id, command_id)` to the bridge, which re-checks
ownership every time. A view resolves to its declared command and uses the same
path. Descriptors are plain data; no framework or process handle crosses the
boundary. `InvokeOutcome` carries provenance (`builtin`, `first-party`,
`third-party`) so the renderer can distinguish trusted builtin output from
untrusted helper text.

## UI contributions

| Slot | Meaning |
| --- | --- |
| `session.action` | Action item on a Session |
| `session.tab` | Additional tab in the Session composition |
| `session.overlay` | Floating bounded overlay; requires an overlay descriptor |
| `session.context-panel` | Side context panel |
| `settings.section` | Settings section |

- Platforms are `desktop` and `ios`; views filter by platform, commands do not.
- Overlay dimensions are absolute pixels from the manifest, clamped by the
  host to the Session view. No full-view takeover, no chrome occlusion.
- The snapshot exposes only `ready` plugins, and commands carry their
  `requires` list so a menu can render a lock for ungranted capabilities.
- Remote Screen is first-party software built on the same grant and lease
  primitives; it is not installed through the plugin registry.

## Registry and installation

- The bridge never scans directories. Plugins load from one explicit registry
  file in the bridge data directory (`<data dir>/plugins/registry.json`,
  overridable by environment for tests and operators), versioned JSON with
  `{id, manifest, helper, enabled}` entries.
- Manifest and helper paths must be absolute and must not contain `.` or `..`.
- The registry, manifest and helper must be regular files with the ownership and
  mode rules above; the manifest id must equal the registry id and the helper
  stem must equal `entrypoint`.
- Duplicate plugin ids, manifest paths, helper paths or command ids make the
  registry ambiguous: the whole registry is refused and the snapshot reports
  the error. A bad entry installs nothing.
- A missing registry means "no plugins", never a discovery fallback.
- Installation starts no process. Enabled entries start after the local
  endpoint is accepting, so a hung helper cannot delay local commands.

## Audit

Every decision is recorded through one audit sink: install, uninstall, manifest
rejection, duplicate registration, enable, disable, enable failure, command
invoked, command denied, unknown command, forged request, helper crash, timeout,
protocol violation, event dropped, grant granted and grant revoked.

Records carry `at_ms`, `plugin_id`, `kind`, `outcome` and a short detail with
ids, capability names and subject. They never carry command arguments, event
payloads, file content or screen data. Every detail passes through the same
redaction helper: control characters (newline, TAB, ESC/ANSI) and bidi
overrides are escaped and the text is capped. Helper-authored strings therefore
cannot forge log lines or grow a record without bound; JSON parse failures are
reported by category and offset without echoing the input.

The audit sink must be bounded and must not block dispatch. Durable audit
persistence and a query surface are part of the M5 evidence gate; a bounded
in-memory sink alone is not durable audit.

## Builtin trust

A builtin plugin is code compiled into the release and reviewed as product
code. It runs in-process, so it can reach bridge APIs directly; that is exactly
why it cannot be installed, upgraded or sourced from a registry. It still
declares capabilities and contributions so the UI, audit and grant model stay
uniform, and its output is tagged `builtin` rather than being passed off as
untrusted plugin text.

## Explicit non-goals

- No plugin marketplace, store, payment flow or public SDK distribution.
- No third-party code inside the bridge's privileged process, by default or by
  manifest flag.
- No claim of OS sandboxing, Wasm isolation or syscall filtering.
- No automatic discovery of plugin directories, and no plugin-driven change to
  the bridge's own lifetime.
- No protocol that can grant itself capabilities, approve actions or control a
  terminal.
- No plugin access to pairing secrets, device private keys or bearer tokens.
- No ANSI-derived plugin events or approvals; events are declared and typed.

## Residual risks

A helper runs with the user's OS identity, so a deliberately malicious helper
can act outside the protocol on files the user can access. Grants limit what the
*bridge* will do for the plugin, not what the OS permits the process. Helpers
can consume CPU/memory until the supervisor's deadlines and kill path react.
Remote invocation depends on the bridge's `plugin.control` scope and the same
revocation rules as other remote commands.

See [security](security.md), [domain model](domain-model.md),
[architecture](../tech-design.md), [implementation plan](../goals/mygo-implementation.md)
and [status](../status.md).
