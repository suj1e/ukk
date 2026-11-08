# ADR-0002: Remote Screen is a session-level plugin

**Status:** Accepted · 2026-11-08

## Decision

Remote Screen is an **opt-in first-party plugin surfaced inside a mobile Agent Session**, not a mode or a top-level destination. It appears as a small movable floating tile in the conversation. Tap to expand to an interactive viewer; minimize to return to exactly the same conversation.

Viewing and control are **separate grants**. Control requires a short-lived single-controller lease, an OS input permission and a visible takeover. Capture has a persistent indicator and a one-tap stop. Screen media uses independent WebRTC/STUN/TURN connectivity; the `ukk-relay` control plane never carries raw video.

The bridge authorizes screen connections and leases; the desktop capture/input adapters and the iOS viewer implement the media endpoints. Screen availability must never affect Agent operation or event delivery.

## Rationale

Most activities are faster and safer over typed Agent events and commands. Direct screen interaction is only for edge cases such as login flows, OS permission dialogs or proprietary GUI applications. A floating tile keeps the Agent conversation primary and avoids product drift into a remote-desktop app.

## Security implications

- Pairing alone grants neither viewing nor control.
- Platform permissions are explicit: Screen Recording and Accessibility on macOS, the equivalent capture/input permissions on Windows/Linux. A permission-flow UI explains and requests them.
- Stop, device lock, backgrounding, lease expiry and revocation immediately tear down capture, media and input.
- The relay sees only opaque control/signalling frames, never screen media.
- Third-party plugins cannot obtain capture/input capabilities without the same durable grants, audit and sandbox boundary.

## Alternatives rejected

- Full-screen remote desktop as the default product: product drift, bandwidth, input friction.
- An embedded stream occupying the conversation permanently: excessive screen usage and navigation distortion.
- Video over Agent JSON events or the relay: latency, backpressure and security problems.
- View and control as one grant: unnecessary privilege and a weaker consent story.

## Consequences

- The plugin/session surface must support an overlay slot, native iOS rendering, session-scoped grants and lifecycle events.
- Capture adapters are platform-specific: macOS first, Windows/Linux where verified, otherwise an explicit unsupported state.
- Media and control are independently testable; the relay can be operated without ever handling video.
- Physical-device media/input acceptance requires signing and a real phone; otherwise it is recorded as **BLOCKED**, not inferred.
