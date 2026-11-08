# Desktop resources

MyGo copies this directory into the packaged app. Platform directories such as
`darwin/` are merged for the matching target.

`darwin/ukk-input` is the macOS Remote Screen input helper. It is compiled from
[`../tools/ukk-input/main.swift`](../tools/ukk-input/main.swift) by the release
workflow and is intentionally not committed. Without it, Remote Screen works
view-only and reports a typed input-unavailable error.
