# herdr-core contract v1

ukk integrates with the external herdr-core (`hc`) through its documented
API; it never copies or rewrites the runtime. This page pins the parts ukk
relies on. The Go client is `internal/hc`; the upstream release is pinned in
`apps/desktop/go.mod` and `docs/tech-design.md`.

## Handshake

ukk starts `hc connect` as a child and speaks JSON-RPC 2.0, one message per
line, until `hello` succeeds:

```json
{"jsonrpc":"2.0","id":0,"method":"hello","params":{"framed":true,"generation":1}}
```

The reply is `hello.schema.json`. Both sides must report `framed: true` and
`generation: 1`; every later message is a binary frame (see `frames.md`). A
core that cannot serve this generation or lacks a required method is an
explicit error, not a degraded guess.

Required methods for ukk: `terminal.run`, `terminal.list`, `terminal.get`,
`terminal.attach`, `terminal.kill`, `terminal.rm`, `events.subscribe`.

## Ownership

The core owns terminals and programs. Closing a ukk view, client or window
detaches; it never sends `kill`. Only explicit user intent with agent-control
may stop a process. A core restart may lose a provider RPC child, but not a
core terminal.

## Terminals

`terminal.list` returns `list.schema.json`; `terminal.run` returns a terminal
record (`terminal.schema.json`) plus optional `output`/`wait`. `address` is the
core identity (`t7`, `vps/t7`). `state` is the core's own vocabulary; ukk
renders it and never invents liveness.

## Events

`events.subscribe` returns the snapshot (`seq`, `terminals`) and a channel of
newline-delimited notifications. A `missed` notification replaces the snapshot;
an `event` with `seq` lower than the last applied one is ignored. Sequence gaps
are detected only for unfiltered subscriptions.

## Input and provenance

Human input and terminal-generated replies are source-tagged
(`DATA` vs `AUTOMATIC_INPUT`) on separate frame kinds. ukk never inspects bytes
to classify input. Observers never write, resize or take focus.

## Backpressure

Every byte read from an attachment is acknowledged with `CREDIT` for exactly
that count. Payloads above 16 MiB are rejected from the header before
allocation. A slow consumer must hit the bounded queue policy and surface
degraded state, not stall the UI.

## Fixtures

- `fixtures/hello.json`
- `fixtures/terminal-list.json`
