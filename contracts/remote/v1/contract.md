# Remote pairing contract v1

Device identity, short-lived pairing and the end-to-end session between a
Swift/Go client and `ukk-bridge`, routed opaquely through `ukk-relay`. The
normative byte-level derivation lives in `apps/desktop/internal/remote/doc.go`;
these schemas pin the JSON shapes and the validation rules.

## Invite

One JSON object, no trailing data, unknown fields rejected
(`invite.schema.json`). All byte strings are unpadded base64url (RFC 4648 §5):
32-byte keys and the invite secret are 43 characters, an Ed25519 signature is
86, an AEAD tag is 22. `relay` is an absolute `wss://` URL with no userinfo.
An invite expires after at most five minutes, is single use and is marked in
progress before any handshake work.

## Handshake

`c1` → `s1` → `c2` (pairing) or `r1` → `r2` → `r3` (resume), each one JSON
object (`handshake.schema.json`). The transcript, HKDF outputs, proofs and the
six-digit code are defined in `internal/remote/doc.go`; a second implementation
must reproduce them with CryptoKit. Pairing succeeds only after the host
verifies `c2.proof`; any failure burns the invite. Resume proves possession with
fresh ephemerals on every connection and never reuses a channel key.

## Envelope

```text
version(1) || stream(4, BE) || counter(8, BE) ||
ChaCha20-Poly1305(k, nonce=counter||stream, aad=header, plaintext)
```

`c2s` and `s2c` keys are separate. Counters start at 1; a bounded replay window
rejects duplicates, old counters and gaps beyond the window. Plaintext above
1 MiB is refused. The relay never sees plaintext or media.

## Grants

Pairing grants nothing. Agent control, screen view, screen control and screen
audio are separate scopes checked per command with expiry and revocation.

## Fixtures

- `fixtures/invite.json`
- `fixtures/c1.json`
- `fixtures/s1.json`
