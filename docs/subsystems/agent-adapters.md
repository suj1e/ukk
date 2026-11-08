# Agent adapters and conversation contract

> Status: **source implemented, fixture-verified only** · contract version 1 · 2026-11-08
>
> Roadmap task: **R2 — Structured Agent adapters and conversation contract**.
> This crate does not touch `crates/host`, `apps/desktop` or live RPC. R1 live
> integration is a follow-up dependency.

## Purpose and non-goals

`crates/agent-adapters` turns documented provider event lines into one
provider-neutral, versioned event contract. It is the structured side of the
architecture rule:

> Prefer typed structured Agent events; never infer approvals/tool execution
> from arbitrary terminal output.

Non-goals of this crate:

- spawning or controlling an agent process;
- owning a terminal, PTY, herdr-core terminal address or process lifetime;
- sending prompts, approvals or RPC responses to a provider;
- rendering UI;
- inferring provider semantics from ANSI terminal text;
- claiming a provider capability that has not been modelled from documented
  fields.

If a provider action is not modelled, the adapter emits a typed
`EventPayload::Unsupported` notice. If the provider or schema version is not
supported, or a line is malformed/oversized, the adapter returns a
`TerminalFallback` and **no structured events**. The Host may keep showing
already-merged structured state and may show the raw terminal, but it must not
synthesize a turn, message, tool call or approval from terminal text.

## Contract version 1

Every emitted event is an `AdapterEvent`:

```text
AdapterEvent {
  contract_version: 1,
  provider: codex_app_server | claude_stream_json,
  provenance: Provenance,
  payload: EventPayload,
}
```

`Provenance` retains the native identities needed for UI correlation, audits
and approval routing:

| Field | Meaning |
|---|---|
| `native_type` | Provider method, notification, message or item type. Sanitized and bounded. |
| `native_id` | Stable provider event/message/request id when documented. |
| `parent_native_id` | Documented subagent parent tool-use id, when present. |
| `session_id` | Provider session/conversation id. |
| `turn_id` | Provider turn id when documented (Claude stream-json does not document one). |
| `item_id` | Provider item/message/tool-call id. |
| `stream_id` | Host-assigned transport stream. |
| `sequence` | Host-assigned line sequence, per stream. |

`EventPayload` covers:

- session start, resume and status;
- turn start/completion with typed status and provider error;
- messages (start/delta/completed) and typed content parts;
- tool calls and tool results, with typed input variants for command, file
  change, MCP, web search and provider-named tools;
- progress (tool output, tool heartbeat, tool input, reasoning summary/raw,
  plan text, retry, status);
- artifacts (file change and turn diff);
- plans and plan text;
- approval requests with native request/item/turn provenance, approval
  decisions and clearance evidence;
- permission denials;
- notices and unsupported actions.

Capability descriptors (`CapabilityDescriptor`) state, per provider and
version, whether each semantic is `Supported`, `Unsupported` or `Unverified`.
`Capability::is_supported()` is false for the latter two.

## Merge and replay semantics

`MergeEngine` consumes parsed events. One parsed transport line is ingested
with `ingest_line(stream, sequence, events)` because one line can produce zero
or several neutral events (a Codex response without modeled events, Claude
`ping`/`message_stop`, or an assistant message with text plus a tool call).

Rules:

1. **Stream identity.** A `StreamId` is one transport connection. A reconnect or
   explicit resume uses a new stream id and starts line sequence at 1.
2. **Zero-event lines.** `ingest_line` always advances the stream cursor. A line
   that parses to zero events is `Skipped`, not a permanent sequence gap.
3. **Out-of-order lines.** A sequence gap buffers later line batches up to
   `Limits::max_pending_events`. Closing the gap drains buffered batches in
   order. Buffer overflow returns `FallbackReason::StreamBufferOverflow`, never
   a silently dropped event.
4. **Replay.** A sequence below the next expected sequence is reported as
   `MergeOutcome::Replay` and is not applied.
5. **Duplicate lifecycle events.** Lifecycle events with a documented native
   identity use an `EventKey` built from payload kind + transport stream +
   native id. An identical payload is `Duplicate` and changes nothing. The same
   key with different content is `Conflict`: the first value is retained and the
   conflict is recorded. Stream scoping prevents JSON-RPC ids reused on a new
   connection from suppressing new approvals.
6. **Repeated updates are not deduplicated.** Plan, plan-text, session-status,
   artifact/diff, permission-denial and tool-start observations are applied in
   stream order. The latest plan/status/diff wins; richer tool starts update
   earlier empty starts; every denial from a result is retained.
7. **Streaming deltas.** Provider deltas have no documented replay-stable id.
   They are applied in sequence order and are **not** deduplicated by content;
   treating repeated text as a duplicate would erase legitimate repetition.
8. **Partial streams.** A completed message is authoritative. A final item
   replaces accumulated partial text; a delta arriving after completion is
   ignored with a reason. Claude assistant lines sharing one `message.id`
   accumulate content blocks so a later tool_use-only line cannot erase earlier
   text.
9. **Resumed sessions.** `SessionResumed` is an explicit event. It preserves
   already-merged history, sets `SessionView::resumed`, and never wipes state
   just because the transport reconnected.
10. **Session mismatch.** A typed event carrying a different session id than the
    established session is rejected with `SessionMismatch`; sessions are not
    mixed silently.
11. **Unscoped typed events.** A typed event without a documented session scope
    is reported as `Unscoped` and not applied.
12. **Unsupported actions.** Unknown provider methods, item types, content
    blocks and server requests become `UnsupportedAction { scope: Action }`.
    Unknown providers/versions fail detection and fall back to raw terminal.
13. **Approvals.** `serverRequest/resolved` confirms that a request was answered
    or cleared, but does **not** state which decision was taken. The merge
    records `ApprovalEvidence::ServerRequestResolved` with no decision. Only a
    documented terminal item status (`declined`) supplies
    `ApprovalEvidence::ItemStatus(Declined)` and decision `Decline`. Approval
    views are keyed by `stream_id + request_id`, so request-id reuse across
    reconnects cannot merge two approvals.
14. **Claude message identity.** `SDKUserMessage.message` (MessageParam) has no
    documented `id`; the adapter uses the event `uuid`, then a stable
    content-derived id. Approval/decision payloads are never inferred.
15. **Bounded state.** Accumulated message/plan text is capped. Progress,
    notices, denials, unsupported actions and conflicts are capped. Dedupe
    entries store a payload fingerprint computed only for identity-deduped
    events. Stream cursors are bounded by `Limits::max_streams` and idle
    streams are evicted without affecting already-merged history; adapter-local
    in-progress accumulation for an evicted stream may reset.

## Redaction and payload bounds

`AdapterConfig` accepts a `Redactor`. `RuleRedactor` performs literal
replacement; it is applied **before** truncation to:

- message text and attachment references;
- command text, cwd, tool arguments and tool output;
- approval reason, command, cwd and grant root;
- file-change diffs and turn diffs.

Errors never include raw provider payloads. They identify the action and field
that failed so diagnostics cannot leak credentials.

Default `Limits`:

| Limit | Default |
|---|---|
| `max_line_bytes` | 1 MiB |
| `max_text_bytes` | 64 KiB |
| `max_input_bytes` | 32 KiB |
| `max_output_bytes` | 64 KiB |
| `max_id_bytes` | 1024 |
| `max_pending_events` | 256 |
| `max_progress_entries` | 512 |
| `max_dedupe_entries` | 4096 |
| `max_streams` | 64 |

An oversized line, id, or missing/invalid required field returns
`ParseOutcome::failed` with `AdapterError` + `TerminalFallback`. Truncated text
is reported through `BoundedText::truncated` / `Message::truncated`; no synthetic
marker is appended to provider text.

## Provider and version matrix

### Codex app-server

Wire contract: JSON-RPC 2.0, `jsonrpc` header may be omitted on the wire.
Adapter supports **wire generation v2** (`thread/*`, `turn/*`, `item/*`).

Detection requires the observed methods `thread/start`, `turn/start`,
`item/started`, `item/completed`. A declared wire generation other than `v2`
is rejected with `UnsupportedVersion`.

| Capability | State | Basis |
|---|---|---|
| Structured turns | Supported | `thread/start`, `thread/resume`, `turn/start`, `turn/started`, `turn/completed`. |
| Messages | Supported | `userMessage`, `agentMessage`, `item/agentMessage/delta`. |
| Tool calls | Supported | `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `webSearch`, `functionCallOutput`. |
| Tool results | Supported | `aggregatedOutput`, MCP `result`/`error`, `functionCallOutput`. |
| Progress | Supported | command output deltas, reasoning summary/raw deltas, plan deltas. |
| Plans | Supported | `turn/plan/updated` and `plan` items. |
| Artifacts | Supported | `fileChange.changes[].diff` and `turn/diff/updated`. |
| Approvals | Supported | `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `serverRequest/resolved`. |
| Resumed sessions | Supported | `thread/resume`; `thread/started` matching the integration resume hint is emitted as `SessionResumed`. |
| Partial streams | Supported | Documented item deltas. |
| Terminal fallback | Supported | Host capability; explicit fallback only. |

Deliberately **not** modelled for Codex v1 of this contract: review-mode items,
collab agent tool calls, image view/generation, sleep items, context compaction,
MCP tool-call progress, permission-profile approvals, MCP elicitations,
`tool/requestUserInput`, and other server requests. They produce `Unsupported`.

Sources:

- <https://learn.chatgpt.com/docs/app-server.md>
- <https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/item.rs>
- <https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/turn.rs>
- <https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs>

### Claude Code stream-json

Wire contract: newline-delimited JSON with `type` + `subtype`/`message`.
Adapter supports the documented **2.x** `stream-json` schema. A declared major
version other than 2, or a non-`stream-json` wire version, is rejected with
`UnsupportedVersion`. Detection requires the observed message types `system`,
`assistant`, `user`, `result`.

| Capability | State | Basis |
|---|---|---|
| Structured turns | Supported | `system/init`, `assistant`, `user`, `result`. |
| Messages | Supported | `assistant`/`user` messages and documented Anthropic content blocks. |
| Tool calls | Supported | `tool_use` blocks; tool kind remains `ProviderNamed` (no heuristic classification). |
| Tool results | Supported | `tool_result` blocks; `is_error` maps to `Failed`. |
| Progress | Supported | `tool_progress`, `system/api_retry`, `stream_event` deltas. |
| Plans | Unsupported | stream-json documents no plan event; plan text is not synthesized. |
| Artifacts | Unsupported | stream-json exposes no structured file artifact; tool input is not reclassified as one. |
| Approvals | Unsupported | stream-json exposes permission denials, not approval requests. Approval prompts are an in-process SDK `canUseTool` control surface not modelled by this adapter. |
| Resumed sessions | Supported | CLI `--resume`/`--continue`; a `system/init` matching the integration resume hint is emitted as `SessionResumed`. Replay semantics are merge-level. |
| Partial streams | Supported | `--include-partial-messages` `stream_event` lines; final assistant message is authoritative. |
| Terminal fallback | Supported | Host capability; explicit fallback only. |

Claude `control_request` lines are explicitly `Unsupported`; this adapter does
not fabricate approval requests from the control channel.

Sources:

- <https://code.claude.com/docs/en/headless.md>
- <https://code.claude.com/docs/en/agent-sdk/typescript.md>
- <https://code.claude.com/docs/en/agent-sdk/streaming-output.md>

## Fixture matrix

Fixtures are checked in under `crates/agent-adapters/fixtures/`. Each provider
directory has a `manifest.json` with fixture version, source URLs and an explicit
`capture_method: hand-authored ... not a live capture`.

| Fixture | Scenario |
|---|---|
| `codex/app_server_v2/turn.jsonl` | Thread/turn lifecycle, user + agent messages, streamed deltas, command tool output, turn completion. |
| `codex/app_server_v2/approval.jsonl` | Command approval request, `serverRequest/resolved`, declined command item. |
| `codex/app_server_v2/resume.jsonl` | `thread/started` matching an integration resume hint. |
| `codex/app_server_v2/unsupported.jsonl` | Unknown notification and unknown item type. |
| `claude/stream_json_2_1/turn.jsonl` | Init, assistant text + `tool_use`, user `tool_result` without `message.id`, success result. |
| `claude/stream_json_2_1/partial.jsonl` | `stream_event` text deltas followed by the authoritative assistant message. |
| `claude/stream_json_2_1/assistant_blocks.jsonl` | Two assistant lines sharing one `message.id`: text then `tool_use`. |
| `claude/stream_json_2_1/partial_tool.jsonl` | Streamed tool_use start + `input_json_delta` followed by the authoritative full tool input. |
| `claude/stream_json_2_1/denials.jsonl` | One result with two `permission_denials` entries. |
| `claude/stream_json_2_1/progress.jsonl` | `api_retry`, `tool_progress`, `permission_denied`. |
| `claude/stream_json_2_1/unsupported.jsonl` | Unknown top-level type, unknown system subtype, `control_request`. |

Test coverage in `tests/fixtures.rs`:

- fixture parse + merge for both providers;
- zero-event lines advancing the cursor and closing gaps;
- duplicate ids, replay sequences and stream-scoped request ids;
- partial streams and final-message authority;
- out-of-order streams with buffered gaps;
- repeated plan, status and diff updates;
- resumed sessions preserving history (Codex + Claude);
- session mismatch rejection;
- unknown actions and unsupported scope;
- approval provenance, no invented decision, declined-item evidence, execpolicy amendment data, and no amendment option without a proposal;
- redaction of messages, tool input/output and approvals;
- unknown provider and unsupported version/wire detection;
- malformed JSON, non-JSON-RPC object, oversized line, buffer-overflow reason;
- bounded text/output truncation and flags;
- Claude assistant blocks sharing a message id, tool results without `message.id`, partial tool input and multiple result denials;
- contract version mismatch fallback;
- fixture manifests are versioned and cite sources.

## Host integration notes (R1 follow-up)

1. Call `detect_adapter(provider, &handshake, config)` with methods/types the
   integration actually observed. On `DetectionFailure`, explicitly switch the
   pane to raw terminal fallback and keep `structured_events_available: false`.
2. For each transport line, call `ProviderAdapter::parse_line(stream, sequence,
   line)`, then `MergeEngine::ingest_line(&stream, sequence, outcome.events)`.
   Always call it, including for zero-event and blank lines, so the sequence
   cursor advances.
3. Start `sequence` at 1 for each new `StreamId`; use a new stream id on
   reconnect. Do not reuse hc/terminal sequence numbers as this sequence.
4. Apply `TerminalFallback` only by showing raw terminal output or an explicit
   unavailable state. Never parse ANSI text into the contract.
5. Approval responses are an outbound, separately authorized host action. This
   crate stores the request and its provenance; it does not answer approvals.
6. Treat `Unsupported` as a product state ("structured view unavailable for
   this action"), not as an empty successful event.

## NOT VERIFIED

- **No live provider runs.** Fixtures are hand-authored from documented fields,
  not captured from a running Codex app-server or Claude Code process. Live
  wire compatibility is unverified.
- **No R1 live integration.** No Host RPC, process spawn, terminal attach or UI
  wiring is present in this change. R1 must verify sequence assignment,
  reconnect and fallback behavior against the real runtime.
- **Exact Codex version range.** Only app-server wire generation v2 is
  supported. Compatibility with every Codex CLI release that claims v2 is not
  verified.
- **Claude version range.** Only 2.x is accepted. The exact minimum patch level
  for each documented field (for example `tool_progress.heartbeat`) is not
  verified; capability tokens from `system/init` are not yet used.
- **Claude approvals.** The in-process SDK `canUseTool` control surface is
  documented but not implemented here. `control_request` remains Unsupported.
- **Claude artifacts/plans.** Explicitly Unsupported; no provider-side artifact
  or plan event is documented for stream-json.
- **Codex decision payloads.** `serverRequest/resolved` does not carry the
  client's decision. Only a `declined` item status is treated as decision
  evidence. Client-sent decisions are not recorded by this crate.
- **Codex unmodelled items.** Review mode, collab tool calls, image/sleep/
  generation items, context compaction, MCP progress, permission-profile
  approvals and MCP elicitations are Unsupported in contract v1.
- **Redaction completeness.** `RuleRedactor` replaces configured literals; it is
  not a credential detector. Hosts must supply rules for their secret formats.
- **Payload/version mutation.** There is no live negotiation for provider
  experimental flags. Experimental methods not in the documented stable surface
  are not requested.

## Security consequences

- No implicit permission widening: an approval request is data, not an action.
  Answering it remains a separately authorized Host operation.
- Unsupported provider/version/action never becomes a typed success event, so a
  UI cannot display a fabricated "approved" or "completed" state.
- Provider errors and adapter errors avoid raw payloads, reducing credential
  leakage in logs.
- Redaction runs before truncation and before state retention, but only for
  configured patterns.
- Closing a view or pane is out of scope for this crate; no adapter event can
  stop a process.

## Related documents

- [Architecture](../tech-design.md)
- [Protocol](protocol.md)
- [Security](security.md)
- [Roadmap R2](../roadmap.md)
