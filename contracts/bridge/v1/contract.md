# Bridge contract v1

The command envelope shared by the local Desktop API and paired remote
devices. One dispatcher validates and authorizes every method, so a remote
device and the local window exercise the same code path with different live
grants.

## Transport

- Local: HTTP `POST /v1/command` with `Authorization: Bearer <token>`; `GET
  /v1/events` is SSE. Unix socket `0600` on macOS/Linux; loopback + `0600`
  token file on Windows.
- Remote: E2E envelope over the relay (`contracts/remote/v1`), then the same
  JSON request/response payloads.
- Request and response bodies are bounded (1 MiB), parameters are decoded with
  unknown fields rejected, and the command deadline is enforced.

## Envelope

Request `{"id","method","params"?}`; response `{"id","result"}` or
`{"id","error":{"code","message"}}`. Exactly one of `result`/`error` is
present. `id` is opaque to the bridge and only correlates the reply.

## Methods

| Method | Params | Result |
| --- | --- | --- |
| `health` | — | `{ok,version}` |
| `workspace.get` | — | Desktop mirror snapshot |
| `workspace.set` | `{snapshot}` | `{ok}` |
| `agent.start` | `{session_id,cwd,adapter?,model?}` | Agent snapshot |
| `agent.snapshot` | `{session_id}` | Agent snapshot |
| `agent.list` | — | `{sessions:[…]}` |
| `agent.prompt` | `{session_id,text}` | `{ok}` |
| `agent.abort` | `{session_id}` | `{ok}` |
| `agent.approve` | `{session_id,approval_id,decision,note?}` | `{ok}` |
| `agent.stop` | `{session_id}` | `{ok}` |
| `repo.status`, `repo.diff`, `repo.file` | session/path | repository view |
| `remote.status`, `remote.invite`, `remote.confirm`, `remote.grant`, `remote.revoke` | pairing fields | pairing state |
| `screen.start`, `screen.answer`, `screen.stop`, `screen.input` | session/media fields | screen lease |
| `plugin.list`, `plugin.install`, `plugin.enable`, `plugin.disable`, `plugin.grant`, `plugin.revoke`, `plugin.invoke` | plugin fields | plugin state |

Agent payloads are `contracts/agent/v1` in snake_case for this boundary
(`providerSessionId` becomes `provider_session_id`; `isError` becomes
`is_error`). `agent.list` entries carry `session_id`, so a client can map every
provider session back to its product Session.

## Authorization

The local Desktop is the same OS user and is authorized by the private endpoint
plus token. A remote device must hold a live grant for the method's scope
(`workspace.read`, `session.observe`, `agent.send`, `agent.approve`,
`filesystem.read`, `git.write`, `screen.view`, `screen.control`,
`screen.audio`, `plugin.control`, `device.manage`). Pairing implies none of
them. Grants are checked per command, not cached from the handshake.

## Errors

`code` is stable: `invalid_params`, `unauthorized`, `unsupported`, `not_found`,
`conflict`, `limit_exceeded`, `internal`, plus the provider codes from
`contracts/agent/v1`. Messages are bounded and never contain tokens, pairing
secrets or screen content.

## Fixtures

- `fixtures/request-start.json`
- `fixtures/response-snapshot.json`
- `fixtures/response-error.json`
