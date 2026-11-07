# Device pairing and remote trust

## Goal

A Mac and iPhone share the same ukk Host and agent sessions without requiring manual ports, VPN or a cloud login by default. Pairing should feel like scanning a QR from Desktop → iPhone, followed by **explicit desktop confirmation**. A device stays paired until revoked.

## Protocol sketch (not final cryptographic specification)

```mermaid
sequenceDiagram
  actor Owner
  participant Mac as Desktop / Host
  participant Relay as Untrusted Relay
  participant Phone as iOS App
  Owner->>Mac: Add Device
  Mac->>Mac: Generate one-time invite, expiry, ephemeral key
  Mac-->>Owner: Show QR (relay origin + host ID + public key + invite)
  Owner->>Phone: Scan QR
  Phone->>Phone: Generate device keypair (Keychain)
  Phone->>Relay: Connect using opaque route token
  Relay->>Mac: Forward handshake (without plaintext secrets)
  Phone<->>Mac: Authenticated key agreement + transcript verification
  Mac-->>Owner: Verify short code, approve phone + scopes
  Owner->>Mac: Confirm
  Mac->>Mac: Persist authorized device public key + scopes
  Mac-->>Phone: Encrypted pairing success
  Phone->>Phone: Store trust material in Keychain
  Phone->>Relay: Reconnect later using device credentials
  Relay->>Mac: Route ciphertext
```

## QR fields (proposal)

```json
{
  "v": 1,
  "relay": "https://relay.example.invalid",
  "host_id": "uuid",
  "host_public_key": "base64url-encoded-public-key",
  "invite_id": "opaque-random-id",
  "invite_secret": "one-time-secret-base64url",
  "expires_at": "2026-11-07T10:00:00Z"
}
```

**Illustrative only.** The invitation is a short-lived bootstrap credential. The final format needs threat modelling, rigorous binding of relay origin/host identity/roles, replay protection, entropy verification, and authentication protocol review. The QR must **never** contain a permanent host private key or a non-expiring bearer token.

## Crypto & identity

- Use independently generated persistent Host and phone identity keys; storage: Mac Keychain and iOS Keychain/Secure Enclave where key algorithm supports it.
- Fresh ephemeral ECDH handshake, transcript binding, key derivation via standard audited library; authenticated AEAD for messages and replay-window handling.
- Ratchet/rotation for sessions when designed; don't claim forward secrecy or E2EE without proving the chosen construction.
- Keys are per-device, not one shared password for all phones. Revoke a device without invalidating every other device.
- Certificate-based TLS to relay plus E2EE on top when host/phone exchange sensitive payloads.
- Use a 2-minute invitation expiry as a UX default; make single use and rate limit enforced server-side.

## Device trust and permission hierarchy

Roles: `observe`, `agent.send`, `agent.approve`, `workspace.write`, `screen.view`, `screen.control`, `filesystem.transfer` are separately granted. Permissions scoped to Host and optionally Space/Session. Desktop's local user can revoke all trust instantly.

Pairing does not grant every permission. Phone may be allowed to view sessions but need extra confirmation for approval actions and screen-control escalation.

## Connection lifecycle

- Host initiates outbound TLS/WSS to relay; no inbound port forwarding requirement.
- Device is either `online`, `reconnecting`, `offline`, or `revoked`.
- On reconnect, authenticate persistent identity then issue a short-lived session token.
- No persistent background WSS on locked iOS; use APNs only for opaque notification hints, fetch authenticated state on foreground.
- Timestamp skew, device loss, failed pairing and compromised relay must be handled explicitly.

## Alma relay feasibility

The MIT-licensed [alma-relay](https://github.com/yetone/alma-relay) supports outbound desktop WSS, HTTP/SSE/WS forwarding, protocol docs, TURN credential support for WebRTC and APNs with an app's own signing key. This **does not mean** Alma's client pairing/E2EE flow is available as an off-the-shelf ukk SDK. We must implement and test our own client pair identities and cryptography over its supported transport, or modify/fork the relay after a compatibility spike. See [transport](PROTOCOL.md).

## Acceptance conditions

An unknown phone cannot subscribe, list projects, send commands, approve, view a screen or control input. Reusing a completed/expired QR fails. Spoofed relay responses fail authenticated handshake. Revoked device access fails even if cached transport credentials remain. User can see and kill active sessions on Mac.

See [security](SECURITY.md).
