# Host lifecycle and local IPC hardening (roadmap H1)

> Implementation record, 2026-11-08. Source and automated tests are committed.
> Evidence is deliberately split by OS; a macOS run is not Linux or Windows
> evidence. See the matrix and **NOT VERIFIED** section at the end.

## Goal

Give the Desktop a Host that starts, restarts and is diagnosed independently of
any GPUI window, while keeping the local metadata/RPC endpoint same-user only,
bounded and recoverable. The Host is **not** the Agent runtime: herdr-core owns
Agents. Host stop/restart/uninstall must never signal herdr-core or an Agent.

## Service artifacts

All artifacts run as the **interactive user**. There is no root daemon, no
LocalSystem service and no global machine-wide install.

| OS | Artifact | Location | Start / restart model |
|---|---|---|---|
| macOS | launchd user agent `com.ukk.agent-host` | `~/Library/LaunchAgents/com.ukk.agent-host.plist` | `RunAtLoad`, `KeepAlive` on unsuccessful exit, `ThrottleInterval` 10s |
| Linux | systemd `--user` unit `ukk-agent-host.service` | `$XDG_CONFIG_HOME/systemd/user/` (default `~/.config/systemd/user/`) | `Restart=on-failure`, `RestartSec=2`, `KillMode=process` |
| Windows | Scheduled Task `ukk-agent-host` | Registered for the current user, `Interactive` / `RunLevel Limited` | At logon; `MultipleInstances IgnoreNew` |

Templates live in `crates/host/service/` and are rendered with the absolute
executable path:

- `service/com.ukk.agent-host.plist`
- `service/ukk-agent-host.service`
- `service/windows/ukk-agent-host.ps1`

`ukk-host serve` **never** installs, enables or modifies a service. Installation
only happens through the explicit commands below.

## Commands

```text
ukk-host install     # register + start the current-user service
ukk-host uninstall   # stop/disable the Host service and remove the artifact
ukk-host restart     # restart the Host process only
ukk-host status      # service state, endpoint state, permissions, stale diagnostics
ukk-host cleanup     # remove a stale Unix socket only after proving no live Host
```

Behaviour details:

- `install` refuses to hide errors. `launchctl bootstrap`, `systemctl --user
  enable --now` and `Register-ScheduledTask` failures are printed with their
  command output.
- `uninstall` and `restart` stop only the Host endpoint. They never send a
  signal to herdr-core, `hc`, or a terminal/Agent process. The Linux unit uses
  `KillMode=process` so a service stop cannot fan out to other processes.
- `status` reports the artifact presence, service-manager state, the endpoint
  path, directory mode/uid, and whether the socket is missing, live, stale or
  occupied by an unrelated file. It does not require the Desktop to be running.
- Upgrades: reinstall/replace the binary at the same path and run `ukk-host
  install` (or let the service manager restart it). The service points at the
  absolute binary path recorded at install time.
- `serve` and `cleanup` acquire an exclusive `flock` on
  `<socket-dir>/agent-host.lock`. The kernel drops this lock when the Host
  exits, so a crash or SIGTERM never leaves an unrecoverable lock.
- On Windows, Host exclusivity is proven by creating the first Named Pipe
  instance **before** `LocalAuth::install()` and before broker work starts. A
  refused second Host therefore cannot rotate the live Host's mutation
  capability token or leave it transiently missing.

## Stale-socket recovery (Unix)

The socket file survives a graceful Host stop (observed with SIGTERM). Recovery
is intentionally conservative and race-free between Hosts:

1. Create/verify the socket directory (default `~/.ukk/run`, mode 0700, owned by
   the current user).
2. Acquire the lifecycle `flock`.
3. Probe the path:
   - missing → bind normally;
   - a socket with a **successful connect** → refuse: a live Host owns it;
   - a socket with **`ECONNREFUSED`** → re-check it is still a socket, verify it
     is owned by the Host uid with mode 0600, then unlink it and bind;
   - any other connect error (`EACCES`, `EMFILE`, `EAGAIN`, …) → abort with a
     diagnostic; ambiguous failures never authorize an unlink;
   - a symlink or any non-socket file → refuse and leave it untouched.
4. Bind, set the socket to 0600, verify type/uid/mode, then serve.

`ukk-host cleanup` validates the directory (real directory, owned by the Host
uid, no group/other bits) before creating the lock, then performs the same
probe under that lock. It only removes a socket that is still owned by the
Host uid with mode 0600. It refuses a live socket, a foreign file, a symlink
and any non-private stale socket.

### Reproduced acceptance scenario

The integration owner reproduced the motivating failure with real herdr-core
0.1.18 and two live terminals (`t1`, `t2`):

1. `ukk-host serve` and `ukk-host request terminal.list` succeed.
2. SIGTERM the Host; `~/.ukk/run/agent-host.sock` remains.
3. A second `ukk-host serve` previously failed with `Address already in use`.
4. `hc ls` still shows both terminals live (Agent lifetime independence).

After this change the second start probes the stale socket, confirms no live
Host, unlinks and binds. While the first Host is **live**, a second Host is
rejected by the lifecycle lock and the live socket is never unlinked.

Real herdr-core evidence (macOS, local): with the installed herdr-core 0.1.18
binary (`~/.local/bin/hc`), the hardened Host returned `connected: true`,
`terminal.list` succeeded, SIGTERM left a stale socket, `status` reported
live then stale, the next `serve` recovered it, `cleanup` refused while the
Host was live and removed the socket only after it stopped. No Agent process
was signalled.

Automated equivalents (macOS, run locally):

- `crates/host/tests/ipc_hardening.rs::sigterm_leaves_a_stale_socket_and_restart_recovers_it`
- `...::live_socket_is_never_replaced_by_a_second_host`
- `...::explicit_cleanup_removes_only_a_verified_stale_socket`
- `...::cleanup_refuses_an_unrelated_file`
- `crates/host/src/local_fs.rs` unit tests for 0700/0600 rejection and
  live/foreign refusal.

## Local endpoint hardening

### Unix socket

- Directory: default path validated (real directory, Host uid) and only then
  forced to 0700; **all** paths (including `UKK_SOCKET` overrides) must be a
  real directory owned by the Host uid with no group/other permission bits.
  Identity is checked before any chmod. Insecure directories and symlinks are
  rejected before bind.
- Socket: created 0600, then type, uid and mode are verified; a mismatch aborts
  startup.
- Peer identity: Linux/Android `SO_PEERCRED`, macOS/BSD `getpeereid`; the peer
  uid must equal the Host's effective uid. Platforms without a verifiable peer
  identity fail closed.
- Timeouts: 60s idle read deadline, 10s write deadline.
- Quotas: 32 concurrent clients, 256 requests per connection, 64 KiB request
  line, 4 MiB reply.

### Windows Named Pipe

- Name: `\\.\pipe\ukk-agent-host-<user SID>`; the name is namespacing, not the
  security boundary.
- Explicit protected DACL: current user + SYSTEM, full access only.
- `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance; a second Host cannot
  take over the live endpoint.
- `PIPE_REJECT_REMOTE_CLIENTS`; no TCP fallback anywhere.
- Client opens with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, never
  full impersonation.
- **Server verifies the client process token SID** and **client verifies the
  server process token SID**; a different user is rejected in both directions.
- All pipe reads/writes are overlapped with a deadline and `CancelIoEx`, so a
  silent or non-reading peer cannot block a worker indefinitely. Server accept
  also uses an overlapped `ConnectNamedPipe`.
- Same quotas/timeouts as Unix: 32 clients, 256 requests/connection, 64 KiB
  request, 4 MiB reply, 60s idle, 10s write.

## Evidence

| Claim | macOS | Linux | Windows |
|---|---|---|---|
| Source compiles | Yes (`cargo test --workspace`, local) | Yes, CI `42392340865` | Yes, CI `42392340865` |
| Framing/JSON/auth/oversize/quota/connection-cap tests | Yes (`crates/host/tests/ipc_hardening.rs`, local) | Yes, CI `42392340865` | Unix-only test file; Windows equivalents are `windows_named_pipe.rs` |
| Socket dir 0700 / socket 0600 | Yes (local test) | Yes, CI `42392340865` | N/A (Named Pipe ACL instead) |
| Peer identity check | `getpeereid` unit tests (local) | `SO_PEERCRED` tests, CI `42392340865` | Server/client SID tests, CI `42392340865` |
| Stale socket + SIGTERM + restart | Yes (local, real process) | Yes, CI `42392340865` | Pipe vanishes with last handle; second-instance + token-preservation test, CI `42392340865` |
| Overlapped read/write deadline + cancellation | N/A | N/A | Unit tests (`windows_pipe.rs`) pass in CI `42392340865` |
| Service template content | Yes (unit tests compile/run on all OS) | Yes (same) | Yes (same) |
| Real `install`/`uninstall`/`restart` on a machine | **NOT VERIFIED** (commands not executed to avoid registering a real user service during development) | **NOT VERIFIED** | **NOT VERIFIED** |
| Real herdr-core CLI integration (`doctor`/`list`/`watch`, Agent survival) | Yes, herdr-core 0.1.18 local, plus CI `42392340910` | CI `42392340910` | CI `42392340910` |
| Host broker `connected: true` over `serve` | Yes, local real hc 0.1.18 | **NOT VERIFIED** | **NOT VERIFIED** |
| Different-user denial | **NOT VERIFIED** (see runbook) | **NOT VERIFIED** (mode check + peer uid source) | **NOT VERIFIED** (see runbook) |

**Cross-platform CI at code commit `9b28bce`:**

- Rust core/Host run `42392340865`: formatting, workspace tests and Clippy on
  `ubuntu-latest`, `macos-latest` and `windows-latest` all passed.
- Real herdr-core acceptance run `42392340910`: `doctor`/`list`/`watch` against
  herdr-core 0.1.18 and Agent survival passed on all three OS images.
- Desktop native cross-platform run `42392340928` passed (validates the shared
  Host client from the Desktop workspace).

Real service-manager execution (register/start/stop via launchd, systemd and
Task Scheduler) and cross-user denial remain NOT VERIFIED.

## Different-user denial (NOT VERIFIED) — runbook

### Windows

1. On a Windows host with two local accounts (`owner`, `peer`), run
   `ukk-host serve` as `owner`.
2. Record the SID (`whoami /user`) and the pipe name
   `\\.\pipe\ukk-agent-host-S-1-5-21-...`.
3. As `peer`, run any program that calls `CreateFileW` on that pipe. Expect
   `ERROR_ACCESS_DENIED` (the DACL grants only `owner` + SYSTEM).
4. Even if the DACL is altered, the Host compares the client token SID and
   rejects with a `PermissionDenied` diagnostic; the client also verifies the
   server SID.
5. Capture the pipe DACL with Process Explorer / `Get-Acl`-equivalent tooling
   and record the OS build.

### Linux

1. Create a private socket directory owned by the Host user with mode 0700.
2. As another uid, attempt to connect. The mode check normally blocks traversal.
3. To exercise the peer-uid check specifically, temporarily allow directory
   traversal (e.g. mode 0711) while keeping the socket 0600, then connect from
   the other uid. Expect the Host log
   `rejected local client owned by uid ...; Host runs as uid ...` and a closed
   connection.

macOS `getpeereid` behaves like the Linux peer-credential case; the directory
mode is the primary barrier.

## Residual risks (explicit)

- A malicious process running as the **same user** is inside the trust domain.
  This transport authenticates the user, not the executable; Windows code
  signing / app identity is future work.
- The mutation capability token is a same-user file (0600 on Unix; the Windows
  profile ACL is relied upon). It is not a defense against same-user code.
- The Windows DACL still includes SYSTEM. The process SID check rejects SYSTEM
  clients, but a higher-privilege service would be a new, independently audited
  boundary; the Host must not run as LocalSystem.
- `install`/`uninstall`/`restart` were not executed against a real service
  manager in this change; template tests and source review only.
- Windows Named Pipe tests, including the overlapped deadline tests, now pass
  in Windows CI, but a real Windows machine and different-user denial remain
  unverified.
- No mounted remote transport and no TCP listener exists here by design.

## Related

- [Windows Named Pipe transport](windows-ipc.md)
- [Security architecture](../subsystems/security.md)
- [Platform support](../subsystems/platform-support.md)
