# Desktop Spike 02 — GPUI native shell

> Status: **source committed; macOS build and real Host integration NOT YET VERIFIED**. As of 2026-11-07, the sandbox environment does not provide Rust/Cargo or an interactive macOS desktop. Avoid saying this UI was compiled or launched until macOS Actions + local test pass.

## Scope of this increment

```text
ukk/
  apps/desktop/
    Cargo.toml         independently pinned GPUI workspace
    src/main.rs        native GPUI shell + Agent selection + Host refresh
  crates/host-client/
    src/lib.rs         bounded, read-only Unix-socket JSON-RPC + typed Terminal
    tests/local_ipc.rs socket round-trip
  crates/host/         existing Host smoke server
  crates/hc-client/    existing hc connect adapter
```

- Desktop: native toolbar, **Local machine** Space placeholder, terminal/agent sidebar with dynamic parent-child grouping, selected-agent overview/runtime metadata, connection state, refresh button and side inspector.
- Reads real `terminal.list` response through **ukk Host**, not directly from herdr-core. Uses fields `address`, `name`, `title`, `argv`, `cwd`, `state`, `parent`, `status` from the published hc schema. No fake sessions or fixed Agent roles.
- The Agent tree is derived from actual hc `parent` links (including orphan/cycle-safe fallback); roles are never preassigned.\n- Input and stream processing stays off the foreground/UI thread. Refresh performs an IPC request through a background executor.
- Explicit **read-only** interface. No agent launch, approvals, chat or terminal input yet.
- Displays `Live · status unknown` when hc has no OSC 7501 status rather than inferring `Working` from text.
- On IPC error, the UI labels Host offline; prior metadata, if retained, must not be mistaken for live status.
- Closing GPUI window does not invoke `hc stop`, `terminal.kill` or stop the independent Host.

## Dependency strategy

GPUI and `gpui_platform` are pinned to Zed commit `b5cd310b3b54dfc659fcc06453e6358843dfe216`, checked on 2026-11-07 against official `hello_world.rs` and `testing.rs` examples. This is a *source/API pin*, not proof that every version combination compiled in ukk. Native GPUI dependencies are kept in a standalone workspace at `apps/desktop`; root `cargo test --workspace` checks the light Rust libraries without fetching a full GPUI graphics toolchain.

## Run on macOS

Install Rust stable, the macOS CLI developer tools/Xcode as required by GPUI, and herdr-core's `hc`. From repository root:

```bash
cargo test --workspace
cargo run -p ukk-host -- doctor

# Terminal 1: start Host (runs until stopped)
cargo run -p ukk-host -- serve

# Terminal 2: test Host
cargo run -p ukk-host -- request terminal.list

# Terminal 3: build and launch the native GPUI app
cargo run --manifest-path apps/desktop/Cargo.toml
```

The Desktop and Host use the same `UKK_SOCKET` override or the default `~/.ukk/run/agent-host.sock`. Run `hc ls` and launch test agents via hc before expecting populated agent rows.

Note that the Host's current socket is created with user-only permissions and **not** a public API; the desktop and Host should run as the same OS user.

## Demonstrated UI behavior (source-level only)

- Click Agent row: changes selected terminal, updates central inspection view.
- Click Overview / Runtime: changes native detail tab.
- Click Refresh: runs a bounded local IPC read without blocking the GPUI event loop.
- Host offline: shows visible error in inspector and connection footer.
- Terminal already finished: UI shows Finished instead of a stale working status.

No real screenshot, successful binary, provider API nor multi-session persistence claim yet.

## Acceptance checklist

- [x] Native GPUI app source with selected-Agent navigation
- [x] Read-only typed Host client and local IPC test source
- [x] Pinned GPUI and gpui_platform source commit
- [x] macOS Actions `cargo check` workflow added (await outcome)
- [ ] GPUI compile passes macOS GitHub Actions
- [ ] App actually launches on Mac and displays multiple real hc terminals
- [ ] UI responsiveness under Host disconnect, stalled socket, reconnect
- [ ] Mac window close/open does not kill running Agent or Host
- [ ] Agent tree and multi-pane UI from persisted ukk Domain Sessions (later)
- [ ] Live event broker with snapshot/gap recovery (later)

## Next steps

1. Verify macOS workflow and fix any GPUI API/dep mismatches. Never claim done before real checks pass.
2. Implement persistent Host event broker, Subscription API, snapshot replay and safe polling/event update on UI.
3. Implement actual app-domain Spaces/Sessions/AgentRuns and parent-child tree derived from hc terminal.address/parent mapping.
4. Add a real tab/split-pane state model that never owns Agent lifecycle.
5. Wire structured Agent adapters for native chat/tools/approval and optional terminal inspector, then Diff/Files.
6. Keep the approved mobile floating Remote Screen concept entirely separate from this Desktop read-only UI spike.

See [PRODUCT.md](PRODUCT.md), [ARCHITECTURE.md](ARCHITECTURE.md), [DOMAIN_MODEL.md](DOMAIN_MODEL.md), [ROADMAP.md](ROADMAP.md).
