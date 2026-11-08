# ukk — Delivery roadmap

> Short roadmap. The master tracking issue is **[#55](https://github.com/suj1e/ukk/issues/55)**; all other prior issues were closed as not planned. Work is tracked there and in the ordered plan, not in a second task queue.

## Direction

Native Go/MyGo Desktop on macOS/Windows/Linux, external herdr-core v0.1.18 for persistent terminals and processes, a Go `ukk-bridge` for authorization/durable state, a self-hostable Go `ukk-relay`, and a native SwiftUI iOS/iPadOS companion. Greenfield reset: no data/schema/protocol migration from the old Rust/GPUI prototype.

## Execution order

The concrete implementation order and evidence gates are [M0…M6 in the MyGo implementation plan](goals/mygo-implementation.md):

- **M0** — repository cutover, single Go module, real MyGo native shell.
- **M1** — bridge IPC, durable workspace, real herdr-core terminals, provenance/resize/backpressure.
- **M2** — provider-neutral Agent workspace with real omp; future provider contracts without fake adapters.
- **M3** — self-hosted WSS/TLS relay, device possession/revocation, native iOS pairing/chat/approvals.
- **M4** — floating Remote Screen with separate view/control grants and independent STUN/TURN media.
- **M5** — third-party plugin sandbox, durable grants/audit and denial/replay hardening.
- **M6** — unsigned `.dmg`, NSIS `.exe`, `.deb`+tar, device-build `.ipa`, and the acceptance matrix.

These stages are implementation/evidence order, not user-confirmation gates. No calendar deadlines are promised.

## Current status

Implementation and verification status is recorded with exact evidence in [status](status.md). Product scope and acceptance live in [requirements](requirements.md) and [v1-complete](goals/v1-complete.md). Architecture is [tech-design](tech-design.md).

## Rules

- Closing any UI never stops an Agent; only explicit agent-control intent may stop execution.
- Never derive approvals or tool events from ANSI; provider semantics only.
- Pairing, agent control, screen view and screen control are separate grants.
- The relay never carries raw video.
- Source presence, a configured workflow or a compiled binary is not acceptance evidence.
