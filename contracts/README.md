# ukk wire contracts

Normative, versioned contracts for every boundary ukk owns. Code implements
these documents; it does not define them.

```text
contracts/
  agent/v1/     provider-neutral Agent session contract (bridge ↔ Desktop/iOS)
  bridge/v1/    local and remote command envelope (Desktop/iOS ↔ ukk-bridge)
  core/v1/      herdr-core generation 1 (ukk ↔ external herdr-core)
  remote/v1/    device pairing and E2E handshake (iOS ↔ ukk-bridge)
  world/v1/     app world document in herdr-core (spaces, tabs, panes)
  manifest.json fixture → schema → Go type map
```

## Rules

1. **Versioned directories are immutable once merged.** `v1` accepts additive,
   optional fields only. A removed field, a changed type, a renamed key, a new
   required field or a new enum member on an input is a `v2`.
2. **Fixtures are normative.** Every schema has at least one fixture in
   `fixtures/`, listed in `manifest.json`. CI compiles each schema, validates
   each fixture and decodes the fixtures that name a Go type.
3. **Unknown fields are rejected on inputs and ignored on outputs** unless a
   schema says otherwise. The bridge rejects unknown command parameters; a
   client ignores fields a newer bridge added.
4. **Closed vocabularies stay closed.** State, role, status, decision and event
   kind enums are the complete set. An unknown value is a protocol error, never
   a guess.
5. **No secrets in fixtures.** Pairing fixtures use obvious test keys and
   never a real identity, token or credential.
6. **No terminal-text semantics.** Approvals, tools and turns come only from
   these typed payloads; ANSI output is presentation.
7. **Bounds belong to the schema.** Where a limit is contractual (frame size,
   payload size, timeline bytes) it is stated in the contract page and enforced
   in code, not left to the transport.

## Versioning checklist

- Add a field: optional, has a stated default, listed in the schema, old
  fixtures still validate.
- Change behavior without changing shape: update the contract page and the
  fixture that exercises it.
- Otherwise: create `v2`, keep `v1` readable by the current server for at
  least one release, and update `manifest.json`.

## Validation

```sh
cd apps/desktop && go test ./internal/contracts
```

The test walks every `*.schema.json`, compiles it, validates every manifest
fixture, and decodes fixture → Go type → JSON for the types listed in the
manifest.
