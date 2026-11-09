# herdr-core framed transport v1

After `hello` confirms `framed: true`, every message is:

```text
[channel u32 LE][kind u8][len u32 LE][payload len bytes]
```

- `len` is bounded: a payload above 16 MiB is rejected from the header before
  allocation and the connection is closed.
- `channel 0` carries JSON-RPC 2.0 responses and requests.
- Non-zero channels carry terminal streams (`terminal.attach`) or event
  notification lines (`events.subscribe`).
- An unknown `kind` on a known channel is skipped; an unknown channel with no
  route is skipped.

## Kinds

| Kind | Name | Direction | Payload |
| --- | --- | --- | --- |
| 0 | `DATA` | both | terminal bytes |
| 1 | `CREDIT` | client → core | u32/u64 count of bytes consumed |
| 2 | `CLOSE` | both | JSON close info (`reason`, `exit_code`, `signal`, `finished_ms`) |
| 3 | `RESIZE` | controller → core | `cols u16`, `rows u16` |
| 4 | `FOCUS` | controller → core | one byte, 1 = focused |
| 5 | `FACTS` | viewer → core | JSON display facts (`fg`, `bg`, `cursor`, `cell`) |
| 6 | `AUTOMATIC_INPUT` | controller → core | terminal-generated reply bytes |

## Invariants

- `CREDIT` acknowledges exactly the bytes delivered to the caller. Credit is
  never sent for bytes that were dropped.
- `DATA` and `AUTOMATIC_INPUT` are distinct; classifying input by inspecting
  bytes is a protocol violation.
- An observe attachment refuses `DATA`, `AUTOMATIC_INPUT`, `RESIZE` and
  `FOCUS` locally before any frame is sent.
- `RESIZE` is applied before the next paint; a client must not paint a stale
  grid.
- A `CLOSE` from the core ends the stream; the client does not invent a
  terminal state from it.
