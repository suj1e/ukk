# Upstream references and integration watchlist

Sources checked on 2026-11-07. Pin exact revisions before implementation. Upstream licenses and product claims can change.

| Upstream | Official source | Why it matters | License / caution |
|---|---|---|---|
| herdr-core | https://github.com/herdrdev/herdr-core | Persistent terminals, multi-machine and agent/child process control; OpenRPC/JSON Schema, event stream | Apache-2.0; status comes from OSC 7501 reporting, not textual heuristics; pin API |
| herdr-app | https://github.com/herdrdev/herdr-app | UX reference only; GPUI + terminal application | Apache-2.0; **not** a dependency or fork target |
| GPUI | https://github.com/zed-industries/zed/tree/main/crates/gpui | GPU-native desktop view framework | GPUI crate Apache-2.0; **other Zed crates may have different licenses** |
| alma-relay | https://github.com/yetone/alma-relay | Self-hosted outbound WSS relay, protocol docs, APNs, optional TURN credentials | MIT; existing Alma client pairing/E2EE semantics aren't ukk SDK; compatibility work required |
| ScreenCaptureKit | https://developer.apple.com/documentation/screencapturekit | macOS capture API | Platform privacy permissions required |
| WebRTC | https://webrtc.org/ | Screen media and secure low-latency transport | Select and review a concrete library/license |
| SwiftUI | https://developer.apple.com/xcode/swiftui/ | Native iOS UI | iOS client shares protocol, not desktop rendering |

## Integration decisions not yet made

- GPUI official crate vs third-party forks, and a tested version of native components.
- herdr-core client embedding vs separate `hc connect` process; default architecture prefers documented external API.
- Codex App Server and other provider-specific structured adapter versioning.
- Whether alma-relay needs a fork or works unchanged with our own compatible client and authentication.
- Exact E2EE primitive/protocol implementation; require security review.
- WebRTC library, TURN deployment, codecs and video permissions on each OS.
- License of ukk itself: **not yet chosen**. Do not assume that all upstream license terms transfer automatically.

See [architecture](ARCHITECTURE.md) and [security](SECURITY.md).
