# Security architecture & threat model

> Planning document; these mechanisms must be implemented, reviewed and tested before remote control is generally enabled. Do not describe the present repository as secure or production-ready.

## Assets to protect

Source code, filesystem and secrets; agent tool execution authority; process/terminal control; approval decisions; screen pixels and user input; device identities; session history; pairing invitations; keys; push tokens; relay metadata.

## Trust boundaries

```mermaid
flowchart LR
  PHONE["Untrusted network / paired phone"] -->|TLS + authenticated E2EE| EDGE["Relay / signalling<br/>assumed compromised"]
  EDGE -->|ciphertext| HOST["ukk Host / local user trust domain"]
  HOST -->|least privilege| HC["herdr-core"]
  HOST -->|permission broker| PL["Isolated plugin helpers"]
  PL -->|OS permission| OS["OS Screen / Input / Filesystem"]
  HOST -->|encrypted at rest where appropriate| KEYS["Keychain / device keys"]
```

The relay is not an authority over desktop actions. Phone pairing grants per-device identity, not root privileges. A compromised or revoked mobile device must not retain full control.

## Local Host IPC and service lifecycle

The local Desktop ↔ Host endpoint is a separate trust boundary from the future
remote protocol. It is local-only, same-user and read-only for Agent runtime
state; workspace metadata mutations require a rotated per-Host capability.
There is **no TCP listener and no TCP fallback**.

### Controls implemented in `crates/host` and `crates/host-client`

| Control | Implementation |
|---|---|
| User-scoped lifecycle | macOS launchd user agent, Linux systemd `--user`, Windows logon Scheduled Task (`Interactive`, `RunLevel Limited`). No root daemon. Explicit `install`/`uninstall`/`restart`/`status`/`cleanup`; `serve` never auto-installs. |
| Agent lifetime independence | Service stop/restart/uninstall only owns the Host endpoint. The systemd unit uses `KillMode=process`; the Windows task ends only the Host. No Host path sends `terminal.stop`/`kill`. |
| Unix directory | Validate real directory and Host ownership **before** chmod, then chmod the default dir to 0700 and re-verify; all paths must have group/other bits clear. Symlinks rejected. |
| Unix socket | Created 0600; type, uid and mode re-verified after bind. |
| Unix peer identity | `SO_PEERCRED` (Linux/Android) or `getpeereid` (macOS/BSD); peer uid must equal Host euid. Unsupported platforms fail closed. |
| Stale socket recovery | Exclusive lifecycle `flock`; only `ECONNREFUSED` is treated as stale. Before unlink the path must still be a socket owned by the Host uid with mode 0600. Ambiguous connect errors, live sockets, symlinks, foreign files and non-private stale sockets are never removed. `cleanup` validates the directory ownership/mode before creating the lock. |
| Windows pipe ACL | Explicit protected SDDL DACL: current user + SYSTEM only. No default-ACL fallback. |
| Windows remote rejection | `PIPE_REJECT_REMOTE_CLIENTS`; pipe name is namespacing, not authentication. |
| Windows single owner | `FILE_FLAG_FIRST_PIPE_INSTANCE` is created **before** `LocalAuth::install()` and broker start. A refused second Host cannot rotate or temporarily remove the live Host's mutation capability token. |
| Windows peer identity | Server verifies the client process token SID; client verifies the server process token SID. Both compare against the current user. Client opens with IDENTIFICATION-only QoS. |
| Deadlines/cancellation | Unix read/write timeouts; Windows overlapped I/O with bounded waits and `CancelIoEx`. A silent or non-reading peer cannot pin a worker. The framed `hc connect` handshake and frame writes also have bounded deadlines and kill/reap the client child on expiry. |
| Resource bounds | 32 concurrent clients, 256 requests per connection, 64 KiB request, 4 MiB reply, 60s idle read, 10s write. Excess clients get a bounded error and are closed. |
| Capability separation | Local mutation token authorizes workspace metadata only. Agent mutation, terminal input and screen control require the separate, default-off `agent-control` capability described below; the workspace token never authorizes them. |

### Security consequences and residual risks

- **Same-user is the trust boundary.** A malicious process running as the same
  OS user has the same token identity and can read the capability file. This
  interface is not a sandbox between same-user applications. Windows
  executable-signature/app identity and stronger isolation are future work.
- The Windows DACL includes SYSTEM for conventional pipe administration. The
  SID verification rejects SYSTEM *clients*, but the Host must never itself run
  as LocalSystem. A SYSTEM/elevated Host would be a new audited boundary.
- The Windows mutation token file relies on the user profile ACL; Unix uses
  0600 inside the 0700 run directory. Neither is protected against same-user
  code.
- **Different-user denial is NOT VERIFIED in CI.** The DACL/mode and peer-uid
  paths are implemented and tested within one user; cross-account tests require
  a second OS account and a documented runbook
  ([HOST_LIFECYCLE.md](HOST_LIFECYCLE.md#different-user-denial-not-verified--runbook)).
- Windows runtime tests (including overlapped deadline cancellation and
  second-Host refusal) are committed but must be confirmed by Windows CI and a
  real machine before Windows evidence is claimed.
- `install`/`uninstall`/`restart` were not executed against real service
  managers in this change. Template contents and rendering are unit-tested.
- Credentials, tokens and Agent content must never be logged. Diagnostics may
  show endpoint paths, modes and uids, which are not secrets.

## Protocol trust boundaries (implemented, S1)

`crates/protocol` now implements and tests the remote authorization and
synchronization contract. Host socket/relay wiring and pairing cryptography are
separate follow-ups; this section describes the controls that exist in source
and are covered by contract tests today.

| Plane | Authentication | Authority |
| --- | --- | --- |
| Local IPC | OS-verified peer user id; transport must supply it | local OS owner; no device grant required, but still typed validation and replay classification |
| Remote (WAN) | registered `(device_id, public-key fingerprint, key generation)` plus explicit scoped grants | only the granted scopes; every command re-checks live grants and leases |

An envelope carries no plane, device id or permission bit. Those are fixed by
the authenticated handshake, so a captured envelope cannot be replayed into a
different trust domain. A device claim on a local binding is rejected, a local
hello on a remote binding is rejected, and an idempotency key first used on one
plane is rejected on the other (`cross_plane`). Idempotency records are also
bound to the authenticated principal, so one device cannot claim another
device's retry record or learn its result.

### Enforced invariants

* **Live authorization.** Cached handshake scopes are not authoritative.
  Revocation, key rotation and grant expiry are re-read on every command, every
  subscription request and every outbound event batch
  (`ProtocolService::authorize_delivery`). A denied batch closes the open
  stream. Revocation also withdraws that device's controller leases.
* **Typed denials.** `unknown_device`, `device_revoked`, `forged_identity`,
  `stale_identity`, `missing_scope`, `grant_expired`, `plane_mismatch`,
  `replay_forbidden`, and the three controller-lease reasons are distinct wire
  values.
* **Durable cursors only.** Per-stream cursors are issued by the event log and
  persist across Host and herdr-core restarts. `hc_seq` and `host_epoch` are
  reserved names and are rejected on durable messages and checkpoints.
* **Snapshot on gap.** A cursor behind retained history forces a cursor-bound
  snapshot instead of silent event loss; a cursor ahead of the head is a typed
  error, never a reset. `EventInbox::apply_snapshot`/`resume_from` positions
  deduplication at the snapshot cursor, so resync does not disable duplicate
  detection. Event-id reuse at a different cursor is detected within the
  bounded recent-id window. Restored logs must continue the retention floor and
  belong to the current generation.
* **No blind replay.** `approval_respond` (human decision) and `pointer_input`
  (ephemeral input) are classified `NeverReplay`. The reconnect queue refuses
  them, so reconnection cannot approve an action or inject input the user never
  repeated.
* **Bounded resources.** Envelope, payload, snapshot, queue, retention,
  idempotency-store and replay-batch limits are validated at startup and
  enforced before work. The idempotency store refuses new keys at capacity
  rather than evicting records, because eviction re-enables duplicate
  execution. In-flight records never expire; completed records become
  tombstones after retention and refuse a late retry with `expired` instead of
  executing. The reconnect queue applies the same retention window and discards
  stale entries.
* **Ordered checks.** Transport binding → generation negotiation → device
  identity → semantic validation → live authorization/lease → idempotency
  lookup. An unauthorized retry cannot discover whether a key exists or what
  its result was.

## Pairing and device trust (implemented, S2 core)

`crates/remote` implements the pairing core and the outbound signalling
boundary; a real WSS relay adapter, OS key storage and iOS UI are separate
follow-ups. See [PAIRING.md](PAIRING.md) for the exact wire fields, prologue
encoding, signalling envelope and dependency record.

| Control | Implementation |
| --- | --- |
| Bootstrap credential | 32-byte one-time PSK in a bounded QR payload (`v: 1`), default 2 min / max 5 min expiry, single use, unknown fields rejected, strict `https`/`wss` relay validation |
| Secret handling | `InviteSecret` is zeroized on drop and never formatted; QR parsing/errors discard payload text |
| Key agreement | `snow` 0.10.0 Noise `IKpsk0` (X25519, ChaChaPoly, SHA-256); QR secret as `psk0`; Host static key pinned from the QR; device static key encrypted in message 1; non-contributory/low-order X25519 keys rejected on both sides |
| Transcript binding | Canonical prologue binds version, Host id, invite id, absolute expiry, relay URL and Host public key; Noise hashes it with every handshake message |
| Human confirmation | Six-digit code derived from the final handshake hash plus a random one-time challenge (60 s cap / invite expiry); `confirm()` compares it in constant time and returns a `ConfirmedAttempt` that `approve()` requires |
| Replay/expiry | Invitation is marked in progress before the first Noise read; a failed handshake burns it; expired and reused invitations fail closed |
| Grants | Explicit subset of requested scopes, additive only, absolute expiry (default 30 days / max 90 days), `device.manage` not grantable through pairing; authority is committed only after an encrypted completion ACK (`PendingPairing` → `complete`) |
| Revocation/rotation | Delegated to the S1 live trust registry; revocation clears grants, rotation bumps the generation and clears grants, and existing sessions fail on their next check |
| Key storage | `DeviceKeyStore` trait; only an in-memory test store ships. OS Keychain/Keyring is **NOT VERIFIED** and must not fall back to plaintext SQLite |
| WAN proof of possession | **NOT IMPLEMENTED / MAJOR GATE.** S2 authenticates the device only during pairing. S1 `accept_hello` trusts a client-supplied `DeviceClaim` on `TransportBinding::remote()`; until host/relay wiring requires a fresh Noise session keyed by the device static key (or a signature over hello), a captured claim can be replayed. Do not expose a WAN listener. |

### Relay signalling boundary (ukk-internal, mock-tested)

`transport::RelayConnection` is the only outbound seam, and
`relay::RelayDriver` is the protocol state machine. The envelope is
**ukk-internal** and is not claimed to be alma-relay-compatible; see the gap
record in [PAIRING.md](PAIRING.md).

| Control | Implementation |
| --- | --- |
| Outbound only | The Host sends `register`; there is no inbound listener, port forwarding or TCP fallback, and the relay never dials the Host |
| Reconnect generation | Monotonic `generation()`; the driver refuses a reused/stale generation and resets all state on a higher one. State never crosses a connection generation |
| Bounds | Encoded envelope ≤ 8192 bytes, opaque pairing payload ≤ 4096 bytes (encoded length prechecked before base64 decode), session id `[A-Za-z0-9_-]{16,128}`. Only the single JSON-object frame form is accepted. Per-peer queues must be bounded; the mock relay caps 64 undrained frames and disconnects the peer on overflow |
| Fail closed | Duplicate registration, unknown route, offline peer, oversized frame, stale generation, wrong Host, out-of-order message, session close and revocation all produce a typed error and close the driver |
| Resume | Not part of v1: no resume field exists, and a frame carrying one is rejected as malformed before the relay/driver acts |
| Relay authority | The relay only routes ciphertext and typed control; it cannot grant scopes, authenticate a device or alter pairing state |
| Test evidence | Synchronous in-memory mock relay (`tests/relay.rs`); no socket, TLS, relay process or external call |

The pairing crate performs no network I/O and adds no cryptographic primitive:
the driver carries opaque Noise frames and only enforces routing, ordering,
version and size bounds.

**NOT VERIFIED in S2:** WAN device-claim proof-of-possession, a real WSS/TLS
relay adapter, relay operator authentication, TURN/media credentials,
real alma-relay exchange, OS protected key storage, physical iPhone, relay
outage/reconnect against a real process and independent cryptographic review.

## Threat scenarios and mitigations

| Threat | Required mitigation |
|---|---|
| QR observed by another person | One-time short expiry, host confirmation with code, signed transcript and replay protection |
| Malicious relay / MITM | Authenticated end-to-end key agreement, device public key pinning, transcript binding, AEAD, TLS |
| Stolen phone | Per-device revocation, Keychain storage, optional biometric gate for approvals/control, short-lived sessions |
| Malicious third-party plugin | Signed distribution, separate helper, brokered scoped permissions, process limits, audit, crash isolation |
| Agent prompt injection asks for broad host powers | Approval, scope-bound tools, no implicit plugin/system capability escalation |
| Unauthorized screen viewing | Distinct screen.view consent, visible local indicator, explicit stop, no always-on capture |
| Input injection | Distinct screen.control scope, time-limited controller lease, audit, re-auth on escalation |
| Replay of `agent.send` or approvals | Idempotency keys bound to command fingerprint, principal and plane; approvals carry a mandatory revision; approvals and pointer input are `NeverReplay` and refused by the reconnect queue |
| Revoked or stolen device keeps acting | Live grant lookup on every command; revocation clears grants, withdraws leases and invalidates existing sessions (`device_revoked`) |
| Forged or stale device identity | Fingerprint plus key-generation verification; rotation bumps the generation, clears grants and invalidates sessions (`forged_identity` / `stale_identity`) |
| Duplicate execution after a dropped ACK | Durable per-stream cursor, bounded client dedup by cursor and event id, and stored idempotency classification; ACK loss causes redelivery, not re-execution |
| Cross-plane privilege escalation | Local IPC and WAN are separate bindings; device claims are rejected locally; idempotency records reject cross-plane and cross-principal reuse |
| Cursor reset or silent event loss | Durable cursor independent of `hc_seq`/`host_epoch`; gap requires a snapshot; cursor beyond head is a typed error |
| Relay resource exhaustion | Size/rate limits, quotas, circuit breakers, event backpressure |
| Notification metadata leakage | APNs opaque hints, no code/file/secrets in push payloads |
| Local privilege escalation | User-scoped background service, OS sandbox/entitlements as appropriate, no root service by default |
| Dependency supply-chain | Pinned hashes/versions, audit licenses and vulnerabilities, reproducible CI, release signing |
| Remote Host compromised | Explicit host trust warning; phone cannot guarantee source-code or agent-output integrity |

## Permission matrix (illustrative)

| Role / Capability | Observe sessions | Send to Agent | Approve dangerous action | View screen | Control input |
|---|---|---|---|---|---|
| Local Desktop owner | Yes | Yes | Explicit local policy | OS permission | OS permission |
| Paired phone, observer | Yes | No | No | No | No |
| Paired phone, operator | Yes | Optional scope | Separate approval grant | Optional explicit scope | Separate temporary lease |
| Plugin (default) | No | No | No | No | No |

The server-side scope set is `session.observe`, `session.manage`, `agent.send`,
`approval.respond`, `screen.view`, `screen.control`, `plugin.invoke` and
`device.manage`. Scopes are additive; holding one never implies another.
Unknown scope strings fail to deserialize rather than widening a grant.
Handshake scopes are advisory UI state only; authorization re-reads the live
registry.

## Agent runtime control capability

Terminal input, interrupt, stop and runtime archive are **not** workspace
metadata edits. They are gated by a separate capability that is disabled until
an operator creates its own token file:

- Default path: next to the local Host socket with extension
  `.agent-control` (for example `~/.ukk/run/agent-host.agent-control`), or
  `UKK_AGENT_CONTROL_TOKEN_FILE`. Windows uses
  `%LOCALAPPDATA%\ukk\run\agent-host.agent-control`; an override may point
  elsewhere but the Host still requires a regular file.
- The file must be a regular file (no symlinks), ASCII, at least 60
  characters, and mode `0600` on Unix. Any other state leaves control
  disabled with a typed reason.
- `ukk-host agent-control-enable` creates it explicitly and refuses to
  overwrite an existing token; `agent-control-disable` removes it.
- The rotated workspace metadata token in `local_auth.rs` is never accepted
  for control. RPC calls carry the control value in a distinct `control`
  field; an `auth` value is ignored for `runtime.*`.

New `runtime.*` RPC methods require `request_id`, `intent: "user"` and the
capability for every mutation. `request_id` is the idempotency key; a
concurrent duplicate gets a typed "in flight" error instead of executing
twice, and a reused id with different content is rejected. The Host keeps at
most 256 completed ids and fails closed when that bound is reached instead of
silently evicting an id that might still be retried. `intent: "user"` is a UI
convention asserted by the caller, not cryptographic proof; the capability is
the actual control boundary. The Host verifies the binding's `core_id`
against the connected herdr-core identity before acting. Successful and
denied control attempts are written to a JSONL audit file
(`UKK_AGENT_CONTROL_AUDIT`, default beside the token) containing the request
id, action, core id, terminal address, intent and outcome — never the token or
terminal contents.

Local-only threat model: the capability is same-user, same-host defense in
depth, not remote authentication and not a boundary against a process that can
already read the user's profile. herdr-core itself has no per-client
permission model; anyone who reaches its socket has the account's shell
access. Remote pairing and device authorization remain separate gates.

Closing a Pane/Tab/Session/window, detaching a framed view or calling
`runtime.detach` is local and performs **zero** runtime calls. There is no
code path from view close to `terminal.kill` or `terminal.rm`.


## Credential management

Use macOS Keychain / iOS Keychain. Host identity and pairing material must never be committed to git or logged. Rotate session keys independently of device trust. Persist only public identities/metadata in SQLite; protect sensitive local data with OS file permissions and storage design review.

## Remote Screen policy

- Viewing and control authorization separate.
- Active sharing indication and kill switch locally.
- Never enable input when no local permission or no per-device grant.
- Input lease expires even if the phone silently disconnects.
- Clipboard, file exchange, audio and recordings have independent grants.
- No frame storage unless explicit user action and retention policy.

## Security test evidence (S1)

| Invariant | Contract test |
| --- | --- |
| Two clients reconnect independently; dropped ACK redelivers and dedups | `tests/sync.rs` |
| Out-of-order delivery, event-id reuse, retention gap → snapshot → inbox continues | `tests/sync.rs` |
| Cursor monotonicity across restarts; non-contiguous/squeezed/generation-mismatched durable log rejected | `tests/sync.rs` |
| Revocation mid-session; revocation/expiry/rotation stop an open stream; leases withdrawn | `tests/security.rs` |
| Stale, forged and unknown identity claims; rotation invalidates sessions | `tests/security.rs` |
| Duplicate/idempotent commands; fingerprint conflict; principal and plane scoping | `tests/security.rs` |
| In-flight retention, completion tombstones, capacity refusal, stale queue replay | `tests/retention.rs` |
| Approval and pointer replay refusal; mandatory approval revision | `tests/security.rs`, `tests/wire.rs` |
| Pointer scope and live/expired/mismatched lease denials | `tests/security.rs` |
| Local IPC vs WAN distinct bindings | `tests/security.rs` |
| Oversized payload/event/snapshot rejection | `tests/wire.rs` |
| Version negotiation mismatch and envelope generation mismatch | `tests/wire.rs` |
| Reserved `hc_seq`/`host_epoch` rejection; unknown fields | `tests/wire.rs` |

## NOT VERIFIED in S1

These are explicitly out of scope for this lane and must not be claimed as
working:

* **Pairing and cryptography.** No QR challenge, key agreement, E2EE, AEAD or
  transport replay protection is implemented here. Device fingerprints are
  registry inputs that S2 pairing must produce and authenticate.
* **Host transport integration.** The crate is not yet called by a live Host
  listener or relay connection; no phone or relay packet test has run.
* **Durable persistence.** Event logs, trust records and idempotency records are
  modeled in memory; SQLite tables/migrations are a follow-up.
* **Controller lease issuance UI.** Leases are API-typed and expiry-tested, but
  no local consent surface exists yet (M1).
* **Rate limiting and fuzzing.** Size/queue bounds exist; per-connection rate
  limits, parser fuzzing and property tests remain release-gate work.
* **Platform IPC.** Unix socket / Named Pipe peer verification and lifecycle
  belong to H1 and are independent of this protocol contract.

## Required release gates

1. Third-party review of pairing/cryptographic construction; prefer established audited protocols/libraries.
2. Fuzz and property testing around input parsers, framing, and cursor replay.
3. Manual permission tests on macOS and iOS including screen denial/revocation.
4. Test malicious relay, lost phone, revoked device, expired QR, device identity substitution.
5. Review licenses and transitive native dependencies for GPUI/herdr-core/media integration.
6. Independent security testing before generally available remote access.

## Reporting

Until a dedicated security address and policy are set, do **not** post vulnerabilities with secrets or exploit details into public issues. Repository owner should add a private disclosure channel before any public release.

See [pairing](PAIRING.md), [remote screen](REMOTE_SCREEN.md), and [plugin architecture](PLUGINS.md).
