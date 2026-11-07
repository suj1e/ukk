# ADR-0002: Remote Screen is a session-level capability

**Status:** Accepted UX direction, implementation proposed  
**Date:** 2026-11-07

## Decision

Remote Screen is a **first-party optional plugin**, surfaced as a tiny movable floating preview in the mobile Agent Session. Tap to expand to an interactive viewer; minimize to return to exactly the same conversation. No persistent top-level Remote Desktop tab or mandatory mode switching.

## Rationale

Most activities are faster and safer over typed Agent events and commands. Direct desktop interaction is only for edge cases such as login flows, OS permission dialogs or proprietary GUI applications.

## Security implications

Pairing alone does not grant viewing/control. Require separate view/control grants, macOS Screen Recording/Accessibility permission, visible capture indicator, short control leases and one-tap stop. Use a low-latency media transport separate from the Agent control/event relay.

## Alternatives rejected

- Full-screen remote desktop as default: product drift, bandwidth, input friction.
- Embedded stream occupying conversation permanently: excessive screen usage.
- Video over Agent JSON events: latency/backpressure/security problems.

## Consequences

Plugin API must support session overlay slot, native mobile rendering, session-specific grants and lifecycle events. Screen availability must not affect Agent operation.
