# Incident runbook

Triage and recovery for the Go Desktop, `ukk-bridge`, herdr-core, provider
children, the relay and Remote Screen. The first rule is preservation: a UI,
metadata or relay problem is never fixed by killing terminals or Agents.

## 0. Preserve Agent work first

Do not run `kill`, `killall`, `systemctl stop`, uninstall or "clean" on
`herdr-core`, `omp` or an Agent child to clear a UI symptom. Closing a Pane,
Tab, Session or the Desktop window must detach only.

Before any restart, capture read-only state:

```sh
# Processes (never signal them)
ps -Ao pid,ppid,etime,command | grep -E 'ukk|herdr-core|omp' | grep -v grep

# Bridge discovery file: paths, PID, version — no token
cat "$HOME/Library/Application Support/ukk/bridge.endpoint.json"

# Workspace document: counts and validity only
python3 - <<'PY'
import json, pathlib, os
p = pathlib.Path(os.path.expanduser("~/Library/Application Support/ukk/workspace.json"))
d = json.loads(p.read_text())
print({k: len(d[k]) for k in ("spaces", "sessions", "agents")}, "layouts", len(d["layouts"]), "revision", d["revision"])
PY

# Relay liveness through its proxy
curl -fsS https://relay.example.com/healthz
```

Adapt data paths: `%AppData%\ukk` on Windows,
`$XDG_CONFIG_HOME/ukk` (or `~/.config/ukk`) on Linux, `UKK_DATA_DIR` when set.
Never paste the contents of `bridge.token`, `bridge.json`, the relay token file
or an identity file into an issue or chat.

## 1. Identify the layer

| Symptom | Likely layer | Read-only check |
| --- | --- | --- |
| Window missing or blank; no native UI | Desktop (`cmd/ukk`) | Relaunch; check process is the Go app, not a stale bundle |
| UI shows offline/stale while Agents keep running | Desktop ↔ bridge local API | `curl` the endpoint health route; inspect `bridge.endpoint.json` |
| Command returns `unauthorized` locally | Bridge local token | Endpoint token path exists, is `0600` and matches the running bridge |
| Workspace shows wrong counts, duplicate or missing Session | `internal/workspace` metadata | Validate `workspace.json`; compare with UI snapshot |
| Terminal frozen; attach fails; no output | herdr-core | Process alive; terminal exists in core; observer/controller state |
| Chat stuck, tools missing, provider error | Provider adapter / omp child | Child PID, bridge snapshot state, bounded error text |
| Phone cannot connect | Pairing, grants, relay | Device state, grant expiry, relay `host_connected`, bridge connection status |
| Phone connects but every command is denied | Grants/revocation | Typed code (`missing_scope`, `grant_expired`, `device_revoked`, `stale_identity`) |
| Screen tile black or input ineffective | Remote Screen / permissions | Capture permission, view/control grant, lease, ICE/TURN state |
| Plugin missing or disabled after crash | Plugin supervisor | Plugin state, audit records, helper PID |

Record UTC time, OS and version, the exact method/request, the typed error code
and the bridge/relay process identity before changing anything.

## 2. Local bridge checks

macOS/Linux (Unix socket):

```sh
DIR="$HOME/Library/Application Support/ukk"
curl -sS --unix-socket "$DIR/bridge.sock" http://ukk/v1/health
```

Windows (loopback endpoint):

```sh
curl -sS http://127.0.0.1:<port>/v1/health
```

Health is unauthenticated and returns only `{"ok":true,"version":1}`. For an
authenticated command, read the token from the endpoint's token path and keep it
out of shell history and logs:

```sh
TOKEN=$(cat "$DIR/bridge.token")
curl -sS --unix-socket "$DIR/bridge.sock" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"id":"diag-1","method":"agent.list"}' http://ukk/v1/command
unset TOKEN
```

Do not include the response's conversation text in public reports unless the
user asks for it. Never add `set -x` around a command that uses the token.

Stale socket vs live bridge:

```sh
DIR="$HOME/Library/Application Support/ukk"
lsof -U | grep -E 'bridge\.sock' || true      # macOS/Linux listener check
PID=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['pid'])" "$DIR/bridge.endpoint.json")
ps -o pid,command -p "$PID"
```

If the endpoint PID is alive and the health route answers, the socket is live:
stop. Remove a socket file only when nothing listens on it and the file is owned
by the current user; the bridge's own startup refuses to delete a live or
non-socket path.

## 3. Metadata checks and recovery

`workspace.json` is written atomically, so a partial file usually means an
external edit or disk failure, not an interrupted write.

```sh
DIR="${DIR:-$HOME/Library/Application Support/ukk}"
python3 -m json.tool "$DIR/workspace.json" >/dev/null && echo "valid JSON"
cp -p "$DIR/workspace.json" "$DIR/workspace.json.bak.$(date +%s)"
```

- **Corrupt document:** the Desktop store refuses to open it rather than
  silently resetting, and the bridge refuses a corrupt mirror. Quit the Desktop,
  preserve the file, restore the last verified metadata backup, then restart;
  restart the bridge only to reload its mirror. Layout loss is acceptable;
  Agent or terminal loss is not caused by metadata recovery.
- **Wrong layout conflict:** a stale client lost the revision race. Reload the
  Session and reapply; do not edit the revision by hand.
- **Duplicate bindings or dangling ids:** the document fails validation. Do not
  "repair" bindings by hand unless the user explicitly approves; keep the file
  and report the validation error with the record ids.
- Never delete `workspace.json` to clear an error; that destroys metadata the
  user asked to keep.

## 4. Restart order

Restart only the layer that failed, from the outside in.

1. **Desktop only:** relaunch. The bridge, herdr-core and Agents keep running.
   Reattach the same Session. Do not rebind an unknown terminal.
2. **Bridge:** confirm the old process is gone and no live listener holds the
   socket, then start the bridge from the same data directory. herdr-core
   terminals survive. A provider RPC child owned by the old bridge may be lost;
   report that as unknown/lost and never claim resume unless the provider
   explicitly supports it.
3. **Relay:** stop and start the relay; hosts and devices reconnect. Relay
   restart must not stop Agents, and its relay-side deny list is empty after
   restart — confirm revoked devices are still rejected by the bridge.
4. **herdr-core:** last resort, and only with explicit user intent. Stopping it
   ends the terminals it owns. Preserve logs and terminal state first, and never
   present this as a UI-level fix.
5. **Plugins:** disable or restart the affected plugin only. A plugin crash must
   never require a bridge restart.

## 5. Security incident

Suspected token, invite or key exposure:

1. Stop collecting logs that contain the secret; do not attach them.
2. Revoke the device through the bridge (`remote.revoke`, or the Devices
   surface). Confirm the next command fails with `device_revoked`.
3. Rotate the relay operator token: generate a new 32+ byte value, update the
   relay token file and `bridge.json`, restart the relay, then confirm hosts
   reconnect with the new token and the old one gets `401`.
4. If a host/device private key is exposed, remove the identity file, re-pair the
   device and treat all prior grants as revoked. Do not reuse the old key.
5. Preserve the audit and relay lifecycle records that exist. Durable audit is
   not implemented yet, so capture in-memory or process logs before restarting
   anything; records contain ids, outcomes and timestamps, never tokens,
   plaintext or screen content.
6. Never widen a grant, disable auth or expose the relay without TLS to restore
   access during an incident.

## 6. Evidence and escalation

Collect:

- exact SHA and version of Desktop, bridge, relay, herdr-core and provider;
- OS/device and how each service was started;
- UTC timestamps and the exact request/method, with the typed error code;
- redacted logs and the minimal state needed to reproduce;
- whether the failure involved a restart, a revoked device, a permission prompt
  or a plugin change.

Open an issue with that evidence and mark unverified claims explicitly. A
fabricated screenshot, device result or "it works on my machine" note is worse
than an honest **NOT VERIFIED / BLOCKED** with the missing prerequisite.

## 7. Stop conditions

- Do not kill herdr-core or Agent children to clear a UI or relay problem.
- Do not delete, truncate or hand-edit `workspace.json` without a preserved
  copy and explicit user approval.
- Do not log or share bearer tokens, invite secrets, private keys, TURN
  credentials, prompt text, terminal contents or screen data.
- Do not rebuild, retag or reuse an artifact while investigating; keep the
  failing artifact and SHA intact.
- Do not claim recovery until the affected layer has been re-observed.

Related: [release runbook](release.md), [release contract](../release.md),
[security](../subsystems/security.md), [domain model](../subsystems/domain-model.md),
[plugins](../subsystems/plugins.md) and [status](../status.md).
