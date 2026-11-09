# Agent contract v1

The provider-neutral Agent session boundary between `ukk-bridge` (which owns
provider children) and every client. Implemented by `internal/agents` and
served by `internal/bridge/agent.*`.

## Model

| Concept | Meaning |
| --- | --- |
| Session | Product identity in `id`; one live provider child per id. |
| Turn | One provider turn with `state`, timestamps and an optional typed error. |
| Message | Visible conversation text; `role` is closed. Thinking is never merged into `text`. |
| Tool call | Provider-reported execution with raw JSON `input`/`output`; never derived from terminal bytes. |
| Approval | Provider-emitted request, answerable by `id` even when `toolName`/`input` are absent. |
| Usage | Provider-reported tokens and cost; `reported: false` means the numbers are absent, not zero. |

## Closed vocabularies

- `state`: `idle`, `running`, `blocked`, `done`, `failed`, `cancelled`, `unknown`.
- `message.role`: `user`, `assistant`, `system`, `tool`, `unknown`.
- `tool.status`: `pending`, `running`, `completed`, `failed`, `unknown`.
- `approval.state`: `pending`, `approved`, `denied`, `cancelled`, `expired`.
- `event.kind`: `snapshot`, `message`, `tool`, `approval`, `usage`, `error`,
  `diagnostic`, `closed`.
- Decisions on the wire: `approve`, `deny`, `cancel`.

`unknown` is used when the provider did not report enough to decide. It is
never replaced by a guess, and `closed` is always the last event on a session
channel before the channel closes.

## Ordering and bounds

- `sequence` is monotonic per session and starts at 1.
- Entries, per-entry bytes and total timeline bytes are capped by the bridge;
  a client must not assume an unbounded history.
- A subscriber disconnect never affects the provider child.

## Errors

`error.code` is a stable machine-readable kind (see `snapshot.schema.json` and
`event.schema.json`); callers branch on the code, never on `message`. Messages
are bounded and redacted.

## Fixtures

- `fixtures/snapshot-done.json` — a completed omp turn with usage.
- `fixtures/event-message.json` — one streamed assistant message.
- `fixtures/event-approval.json` — a provider-emitted approval request.
