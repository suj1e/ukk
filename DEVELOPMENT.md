# Development

## Prerequisites

- Rust stable (edition 2021) with `rustfmt` and `clippy`.
- `just` for the command entry points.
- `hc` from herdr-core 0.1.18+ on `PATH` (or `UKK_HC_BIN`) for real runtime work.
- `omp` for the structured Agent provider (or `UKK_OMP_BINARY`).
- macOS: full Xcode with the Metal Toolchain for release GPUI builds.
- Linux: the native packages listed in `.github/workflows/desktop-cross-platform.yml`.
- iOS: XcodeGen + Xcode; see `apps/ios/README.md`.

The Host works without a GPU; the Desktop needs a native display backend.

## Layout

```text
apps/desktop/     GPUI client (standalone workspace)
apps/ios/         SwiftUI companion
crates/           host, domain, store, protocol, adapters, plugins, remote, media, omp
plugins/          sample and privileged helpers
docs/             requirements, design, contracts, runbooks, spikes, decisions
scripts/          docs/version checks and packaging
```

## First run

```bash
just fetch            # dependencies for both workspaces
just check            # docs + fmt + tests + clippy + desktop check
just host             # foreground Host on the default local endpoint
just desktop          # GPUI client (separate terminal)
```

Real integration:

```bash
export UKK_DB_PATH="$PWD/.dev/ukk.sqlite3"
export UKK_SOCKET="$PWD/.dev/host.sock"
export UKK_HC_BIN="$(command -v hc)"
export UKK_OMP_BINARY="$(command -v omp)"
./target/debug/ukk-host agent-control-enable   # optional; enables omp/runtime mutations
./target/debug/ukk-host serve
```

`UKK_AGENT_CONTROL_TOKEN_FILE`, `UKK_AGENT_CONTROL_AUDIT` and the plugin-control
token live beside the default data directory. Never commit them.

## Commands

| Command | Purpose |
| --- | --- |
| `just fmt` / `just fmt-check` | format both workspaces / verify formatting |
| `just test` | `cargo test --workspace` |
| `just clippy` | strict workspace Clippy |
| `just desktop-test` | GPUI native unit tests with the CI shader escape hatch |
| `just desktop-check` | Desktop format + check |
| `just docs` | markdown link/structure/AGENTS-length check |
| `just check` | all required local gates |
| `just package` | unsigned local archive (macOS stages `ukk.app`; set version/arch as needed) |

## Platform notes

- macOS release Desktop: default features require `xcrun metal`; install the
  Metal Toolchain. `gpui_platform/runtime_shaders` exists only for CI smoke and
  must not be published.
- Windows: Named Pipe Host IPC, SID-scoped pipe ACL and overlapped I/O. Real
  different-user denial still needs a machine-level check.
- Linux: X11/Wayland dependencies are mandatory for the Desktop; Host tests do
  not need a display.
- iOS: fixture-first; real pairing/Host/APNs/camera remain **NOT VERIFIED**.

## Verification rules

- Unit/mock tests are not real-daemon or real-device evidence.
- Run the command you changed; a green build is not proof of behavior.
- After a bug fix, add a failing-before/passing-after regression test when practical.
- Record exact commands and outcomes in the PR; mark unverified items explicitly.
- Never stop a herdr-core terminal or Agent to make a test pass.

See [coding-standard.md](docs/coding-standard.md), [status.md](docs/status.md) and
the [host runbook](docs/runbooks/host-lifecycle.md).
