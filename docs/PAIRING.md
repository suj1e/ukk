# Device pairing and remote trust

> Status: S2 pairing/device-trust **core implemented** in `crates/remote` ·
> 2026-11-08. Relay networking, OS key storage, iOS UI and end-to-end media are
> **not** part of this slice. Do not describe this as production-ready
> cryptography; no external security review has happened.

## Goal

A Mac and iPhone share the same ukk Host and agent sessions without manual
ports, VPN or a cloud login by default. The user scans a QR code from Desktop
to iPhone, then **explicitly confirms a short code on the desktop**. A device
stays paired until revoked.

## Implemented boundary

| Piece | Status |
| --- | --- |
| `crates/remote` QR invitation, Noise handshake, confirmation code | implemented and tested |
| Scoped device grants layered on `crates/protocol` trust registry | implemented and tested |
| `DeviceKeyStore` trait + in-memory test store | implemented |
| OS Keychain / iOS Keychain / Secure Enclave adapter | **not implemented** |
| Relay adapter, outbound WSS, reconnect | **not implemented** (trait only) |
| iOS QR scanner, desktop confirmation UI | **not implemented** |
| Real relay/iPhone/security review | **NOT VERIFIED** |

## QR payload v1

The exact wire shape is:

```json
{
  "v": 1,
  "relay": "https://relay.example.invalid",
  "host_id": "…",
  "host_public_key": "base64url(x25519, 32 bytes)",
  "invite_id": "…",
  "invite_secret": "base64url(32 random bytes)",
  "expires_at_ms": 1762550000000
}
```

Rules enforced by `PairingInvite::decode` / `encode`:

* JSON object with **unknown fields rejected**, required fields typed, and the
  whole payload bounded to 2048 bytes.
* `v` must be exactly `1`.
* `relay` must parse as `https` or `wss`; userinfo, fragments, query strings
  and port `0` are rejected. `http`, `ws`, `file` and relative forms never
  parse.
* `host_id` and `invite_id` use the S1 opaque-id validation (1–128 printable
  ASCII bytes); random ids are UUIDv4 (122 bits).
* `host_public_key` and `invite_secret` are canonical **unpadded** base64url of
  exactly 32 bytes. Non-canonical spellings and padding are rejected.
* `expires_at_ms` is absolute Unix epoch milliseconds and must be positive.
  Invitation TTL is capped at 5 minutes; the default is 2 minutes.
* The invitation is single use. A started, completed or failed invitation is
  never reopened for a second attempt.
* `Debug` and `Display` never print the secret. Error values carry no payload
  text. `InviteSecret` is zeroized on drop and does not implement display-like
  accessors.

The QR never contains a permanent Host private key or a non-expiring bearer
token.

## Noise handshake

Implementation: `snow` 0.10.0 (`Apache-2.0 OR MIT`) with the default RustCrypto
resolver. No cryptographic primitive is hand-rolled.

```
Noise_IKpsk0_25519_ChaChaPoly_SHA256
```

* **Roles:** the phone is the `IK` initiator; the Host is the responder.
* **Host static key pinning:** the initiator sets `remote_public_key` from the
  QR. A relay or QR attacker cannot substitute another Host key.
* **Device static key:** the phone's X25519 static key is carried encrypted in
  the first `IK` message; the Host recovers it from `get_remote_static()`.
* **PSK:** the 32-byte QR `invite_secret` is installed at PSK slot `0`.
  `psk0` mixes it before the first ephemeral, so a wrong secret fails on the
  first message instead of leaking a later oracle.
* **Prologue / transcript binding:** both sides hash this canonical byte string
  before any handshake message:

  ```
  "ukk-pairing-prologue-v1\0"
  || u8(version)
  || u32len(host_id)   || host_id
  || u32len(invite_id) || invite_id
  || i64be(expires_at_ms)
  || u32len(relay)     || relay
  || u32len(host_pub)  || host_pub
  ```

  Noise folds the prologue and every handshake message into its handshake hash.
  Changing the Host id, invite id, expiry, relay origin or Host key therefore
  makes the handshake fail. The invite secret is not placed in the prologue.
* **Invite selection:** the phone first sends a bounded `PairingRequest`
  (`v`, `invite_id`). The Host consumes that invitation atomically, then builds
  the responder with the stored secret and transcript. The request itself
  carries no secret.
* **One-time challenge:** after decrypting the phone hello, the Host returns a
  random 32-byte challenge with `expires_at_ms = min(invite expiry, now + 60s)`.
  The phone rejects a challenge that extends past the invite lifetime or is
  already expired.
* **Confirmation code:** both sides derive six decimal digits:

  ```
  SHA-256("ukk-pairing-confirmation-v1\0" || handshake_hash
          || challenge_nonce || i64be(challenge_expires_at_ms))[0..4] mod 1_000_000
  ```

  The desktop displays the code; the user compares it with the phone before
  approving scopes. The code binds the full transcript, both static keys and
  the challenge.

Replay, expiry, wrong PSK, wrong Host key, tampered frames and transcript-field
changes all fail closed with a typed error. A failed handshake burns the
invitation.

**Availability note:** the invitation id is selected before the PSK handshake,
so an attacker who observed the QR can consume the invitation without knowing
the secret. This only burns a short-lived invite; the user can generate another.
Relay rate limiting and route-token isolation are transport work and are not
implemented here.

## Device trust and grants

`crates/remote::DeviceTrust` owns grant issuance and delegates identity
decisions to `ukk_protocol::TrustRegistry`:

* The phone requests explicit scopes. The local user approves a **subset**; the
  Host rejects an approval that adds a scope the phone did not request
  (`scope_escalation`), and the phone rejects a Host payload that does the
  same.
* Granted scopes are independent and additive: `session.observe`,
  `session.manage`, `agent.send`, `approval.respond`, `screen.view`,
  `screen.control`, `plugin.invoke`. Holding one never implies another;
  `screen.control` and `approval.respond` remain separate from observe.
* `device.manage` is **not grantable through pairing**. A remote device must
  not enroll, rotate or revoke other devices; that authority stays local.
* Every grant is bound to `(device_id, public-key fingerprint, generation)` and
  one absolute expiry. The default TTL is 30 days; the maximum is 90 days.
* Revocation clears grants and makes `authenticate`/`live_grants` return
  `device_revoked`; existing sessions fail on their next command because the
  registry is re-read rather than cached.
* Key rotation bumps the generation, clears every grant and returns
  `stale_identity` for old sessions. Re-granting is required.

Device private keys are persisted only through `DeviceKeyStore`; this slice
ships an in-memory test implementation. An OS adapter must use Keychain/Keyring
and must not store the private key as plaintext in SQLite.

## Relay boundary

`crates/remote::transport::RelayTransport` is an opaque, ordered,
reliable, outbound frame channel. The pairing core does not know a relay wire
format. Adapters must enforce:

* Host dials out; no inbound listener, port forwarding or TCP fallback.
* TLS with platform trust-store validation; no insecure-skip-verify.
* The relay sees routing metadata and ciphertext, never the invite secret,
  private static keys or grant contents.
* Frames are bounded by `MAX_PAIRING_FRAME_BYTES` (4096); oversized frames are
  rejected before buffering.
* A relay reconnect is a new pairing attempt. Started or completed invitations
  are never replayed.

### Alma relay compatibility record

Source-inspected only; this is **not** a live compatibility test.

* Repository: <https://github.com/yetone/alma-relay>
* Pinned revision: `06594179b7dc44a500e0c53e814314426bcb9241` (package version
  1.3.0, MIT licensed).
* Its `docs/PROTOCOL.md` states: desktop agents connect with outbound WSS to
  `/agent`, phones reach a desktop through HTTPS/WSS `/c/<token>/…`, routing
  tokens are `[A-Za-z0-9_-]{16,128}`, the relay routes by `sha256(token)` and
  treats everything above its frame as opaque. Alma's own E2E layer rides
  inside that tunnel; it is not a reusable ukk pairing SDK.
* **Compatibility status: NOT VERIFIED.** No relay was run, no frame was
  exchanged, and no ukk adapter was written. Mapping `PairingRequest` and Noise
  frames onto a relay WebSocket (`wmsg`/`wopen`) is future S2 transport work,
  not part of this PR.

## Crypto and dependency record

Direct dependencies of `crates/remote` at the time of this change:

| Crate | Version | License |
| --- | --- | --- |
| `snow` | 0.10.0 | Apache-2.0 OR MIT |
| `serde` / `serde_json` | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 |
| `zeroize` | 1.9.1 | Apache-2.0 OR MIT |
| `base64` | 0.22.1 | MIT OR Apache-2.0 |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 |
| `getrandom` | 0.3.4 | MIT OR Apache-2.0 |
| `x25519-dalek` | 2.0.1 | BSD-3-Clause |
| `url` | 2.5.8 | MIT OR Apache-2.0 |
| `ukk-protocol` | workspace path | same repository |

Notable transitive crates: `curve25519-dalek` 4.1.3 and `subtle` 2.6.1
(BSD-3-Clause), `chacha20poly1305` 0.10.1 (Apache-2.0 OR MIT), `idna` 1.1.0
(MIT OR Apache-2.0).

`crates/remote` is intentionally free of network and I/O beyond the
`DeviceKeyStore` trait.

## What the tests prove

`crates/remote/tests` (52 tests including unit tests):

* invite round-trip, strict unknown/missing field rejection, oversized and
  trailing input, unsafe relay schemes, secret redaction and zeroize-on-demand;
* expiry, unknown invite, single-use replay, wrong PSK, Host key substitution,
  Host id/invite id/expiry/relay transcript mismatch;
* Host/phone confirmation-code agreement, transcript/prologue agreement and a
  deterministic `IKpsk0` handshake hash/confirmation vector;
* tampered and oversized handshake frames;
* approval scope-escalation denial, `device.manage` rejection, grant TTL
  bounds and client-side grant expiry;
* client rejection of a decrypted success payload that names another Host,
  carries an unrequested or forbidden scope, or is already expired;
* revocation and key rotation invalidating existing sessions, independent
  scopes and fingerprinted device binding.

**NOT VERIFIED:** real alma-relay interoperability, OS Keychain/Keyring storage,
a physical iPhone, relay outage/reconnect, and independent security review.
