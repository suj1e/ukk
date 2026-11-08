# Incident runbook

## First: preserve Agent work

Do not kill herdr-core, Host-owned omp children or terminals to clear a UI
problem. Closing the Desktop, Pane or Tab must not stop an Agent. If the Host is
wedged, collect state before any restart.

## Triage

1. Identify the layer: Desktop window, local Host RPC, SQLite metadata,
   herdr-core terminal, omp child, relay/pairing or screen plugin.
2. Record UTC time, app versions (`ukk-host --version`, `ukk-desktop --version`),
   OS, endpoint and the exact failing request/response.
3. Run read-only diagnostics:

   ```bash
   ukk-host status
   ukk-host doctor
   ukk-host workspace-dump
   ukk-host db-doctor
   ```

4. For a repeated Desktop error, capture the status banner text and the Host
   audit entry (never a token).

## Recovery

- **Desktop crash:** relaunch; the Host and Agents keep running. Reconnect using
  the same Session. Do not rebind an orphan terminal without verified core ID.
- **Host crash:** check for a stale socket/pipe, verify no live Host owns it, then
  restart the Host. The current slice does not reattach omp sessions; start a new
  one and never fake resume.
- **Metadata corruption:** back up the SQLite file, run `db-doctor`, restore the
  last verified backup per [host-lifecycle.md](host-lifecycle.md). Layout loss
  must not terminate Agents.
- **Security event:** revoke the device grant or rotate the agent/plugin token;
  preserve audit logs; do not widen permissions to restore access.

## Escalation

Open an issue with the evidence and mark unverified claims explicitly. For
credential exposure, stop collecting logs that contain secrets and rotate the
credential before sharing anything.
