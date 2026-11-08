# Release artifacts / 发布产物

> **Unsigned development artifacts, not a shipped release.** This page defines
> what the release workflow builds, what each artifact contains and what is
> explicitly not claimed: no distributable signature (the macOS bundle is only
> ad-hoc signed), no notarization, no Gatekeeper acceptance, no App
> Store/TestFlight distribution, no auto-update and no Windows service install.
> Artifact evidence belongs in [status](status.md); this page is the packaging
> contract.

## Pipeline

`.github/workflows/release.yml` (`Release (unsigned)`) runs on manual dispatch
and on `v*` tags. It never creates a GitHub Release, applies a Developer ID
signature, notarizes, publishes to a store or uploads anywhere except GitHub
Actions artifacts.

| Job | Runner | Target | Produces |
| --- | --- | --- | --- |
| Desktop macOS | `macos-latest` | `darwin/universal` | `ukk.app` + `.dmg` |
| Desktop Windows | `windows-latest` | `windows/amd64` | `.exe` + NSIS setup `.exe` |
| Desktop Linux | `ubuntu-latest` | `linux/amd64` | binary, `.desktop`, `.deb`, `.tar.gz` |
| iOS | `macos-latest` | `generic/platform=iOS` | unsigned `.ipa` + `.sha256` |

Before packaging, each desktop job runs `gofmt -l .` (must be empty),
`go vet ./...` and `go test ./...` in `apps/desktop`. Packaging is done by the
pinned MyGo tool. Each desktop job writes `VERSION.txt` at the artifact root
(`dist/desktop`), then a `SHA256SUMS.txt` beside it covering every file beneath
that root except itself.

## Build

Desktop app packaging uses only MyGo from the Go module:

```sh
cd apps/desktop
go tool mygo build -platform darwin/universal -o ../../dist/desktop
go tool mygo build -platform windows/amd64  -o ../../dist/desktop
go tool mygo build -platform linux/amd64    -o ../../dist/desktop
```

MyGo and Go are pinned by `apps/desktop/go.mod` (the `tool` and `go`
directives) at MyGo `v0.8.3` and Go `1.27.3`. Packaging requirements per host:

| Host | Requirement |
| --- | --- |
| macOS | Xcode command line tools; the `.dmg` is only built on macOS |
| Windows | `makensis` on `PATH`, or network access so MyGo can download NSIS for the installer |
| Linux | GTK 3, xkbcommon, Wayland/X11, fontconfig, FreeType, GL and Vulkan development packages; `makensis` when cross-packaging the Windows installer |

`go tool mygo build` writes each target under `dist/desktop/<os>/<arch>/`:
the macOS bundle and disk image, the Windows executable and setup program, or
the Linux executable, desktop entry, `.deb` and tarball.

NSIS handling differs by packaging host. On Windows, MyGo downloads NSIS into
its cache when `makensis` is not on `PATH`; a failed download fails the build,
so do not assume the installer is optional. On macOS and Linux, the Windows
installer is skipped when `makensis` is absent, which is why the workflow
installs it there. An artifact set without `ukk-setup-*.exe` fails the release
check rather than counting as a smaller release.

### Release set composition

The product release set is the desktop package **plus** the background and relay
binaries built from the same module:

```sh
cd apps/desktop
go build -trimpath -o ../../dist/desktop/<os>/<arch>/ukk-bridge ./cmd/ukk-bridge
go build -trimpath -o ../../dist/desktop/<os>/<arch>/ukk-relay  ./cmd/ukk-relay
```

Release builds must report the same version as the desktop package; stamping
that value into the bridge/desktop entrypoints is required before the release
check can pass.

The relay is operator software; it belongs in the release set for self-hosting
but is never installed by the desktop package or started by the app. Both
binaries must build without importing the MyGo UI. A tree in which
`cmd/ukk-bridge` is absent cannot produce a complete package; the missing
entrypoint blocks release, it does not exempt the artifact from containing it.

The release workflow builds both binaries into `bin/` beside the packaged app
and verifies their `--version` before checksums. The Desktop also embeds a
`--bridge` mode, so closing the app or a missing sibling binary does not lose
Agent control; the separate binaries serve headless and operator deployments.

### Configuration identity

`apps/desktop/mygo.json` is the single packaging configuration:

| Key | Value |
| --- | --- |
| `name` / `slug` | `ukk` / `ukk` |
| `identifier` | `dev.ukk.desktop` |
| `version` | `0.1.0` |
| `main` | `./cmd/ukk` |
| `icon` | `../../assets/icon/ukk-macos-1024.png` (relative to the module) |
| `out` | `../../dist/desktop` (resolves to `dist/desktop`) |
| `license` | MIT |
| macOS minimum | 13.0 |
| macOS signing identity | `-` (ad-hoc) |
| Linux webview | disabled (native UI only) |

`python3 scripts/check_versions.py` (part of `just check`) enforces those values
and requires the iOS `MARKETING_VERSION` in `apps/ios/project.yml` and the
generated `project.pbxproj` to match the desktop version.

## Artifacts

| Platform | Artifact | Contents |
| --- | --- | --- |
| macOS | `ukk-<version>-<arch>.dmg` containing `ukk.app` and `bin/` | Native Desktop app bundle, ad-hoc signed; `bin/ukk-bridge` and `bin/ukk-relay` |
| Windows | `ukk-setup-<version>-<arch>.exe` (NSIS) and the loose `.exe` | Desktop, Start Menu entry, uninstaller; `bin\ukk-bridge.exe` and `bin\ukk-relay.exe` |
| Linux | `.deb` and `.tar.gz` plus the loose binary and `.desktop` entry | Desktop, app metadata; `bin/ukk-bridge` and `bin/ukk-relay` |
| iOS | `ukk-unsigned.ipa` + `ukk-unsigned.ipa.sha256` | Device build of `UkkCompanion.app` with no signature |

The iOS job runs `apps/ios/scripts/fetch-webrtc.sh` (checksum-pinned,
retrying) before generating the project, because the LiveKit WebRTC xcframework
is not committed.

These files are uploaded as GitHub Actions artifacts with a 14-day retention.
They are development artifacts, not a download page. Anything published later
must repeat the labels and checks below.

### Version identity

- The tag is the release name: `v0.1.0` produces `VERSION.txt` value `0.1.0`;
  a build without a `v*` tag is labelled `0.1.0-dev.<7-char sha>`.
- The embedded version comes from `mygo.json`. A release tag must therefore
  equal the `mygo.json` version; changing one without the other produces a
  binary whose reported version disagrees with the tag.
- `VERSION.txt`, `SHA256SUMS.txt`, the tag, the CI run SHA and the binaries'
  reported version must all name the same version. Any mismatch is a stop
  condition, not a note.
- `ukk-relay` currently answers `-version` with `ukk-relay 0.1.0` and exits
  without listening. The desktop and bridge entrypoints must expose an
  equivalent version flag before a release is claimable; until they do, their
  identity comes from the package name, `VERSION.txt` and the CI SHA.
- No binary may open a window, start a service, bind a listener or contact a
  relay when asked only for its version.

### Checksums

`SHA256SUMS.txt` is generated in the artifact directory over every regular file
except itself:

```text
<64-hex sha256><two spaces><path relative to the artifact root>
```

Requirements:

- UTF-8, `\n` line endings only — no CRLF — so `sha256sum -c` and
  `shasum -a 256 -c` both work.
- Sorted by path relative to `dist/desktop`; nested bundle files appear with the
  target prefix, for example `darwin/universal/ukk.app/Contents/Info.plist`.
- The iOS job writes one `<artifact>.sha256` beside the `.ipa`.

### macOS signing

The bundle is **ad-hoc signed** (`signingIdentity: "-"`) when packaging runs on
macOS. That is not a Developer ID signature, it is not bound to the bundle
metadata in a way Gatekeeper trusts, and there is no notarization or stapled
ticket. `spctl --assess` should reject the bundle, and a first launch from a
downloaded copy is normally blocked by Gatekeeper; verify and record the actual
result instead of assuming it. Instructions for bypassing Gatekeeper are not
part of the release. The bundle carries only the ad-hoc signature produced by
the packaging step; no Developer ID or notarization identity is used.

### iOS IPA

The job generates the project with `xcodegen`, archives with
`CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=""`, and
zips `Payload/UkkCompanion.app` into `ukk-unsigned.ipa`.

- The app is **not signed** and **cannot be installed** on an iPhone.
- It is not TestFlight, not App Store and not a device acceptance result.
- Installing requires a later signing and provisioning path outside this
  document.
- Simulator builds and unit/UI tests are not camera, APNs, Keychain or
  physical-device pairing evidence.

## Verification before publishing a build

1. `gofmt -l .` empty, `go vet ./...` and `go test ./...` green on the exact
   SHA, plus CI green for that SHA.
2. Every expected artifact exists for its platform (`.dmg`, NSIS setup `.exe`,
   `.deb` and `.tar.gz`, unsigned `.ipa`).
3. `VERSION.txt`, `SHA256SUMS.txt`, the tag and the reported binary versions
   agree.
4. `sha256sum -c SHA256SUMS.txt` passes on a clean copy.
5. The macOS bundle launches its native window from the extracted artifact and
   the app is labelled unsigned in the run notes.
6. `ukk-bridge` and `ukk-relay` are present in the staged release set. A
   binary lacking a version flag is a release gap to fix, not a pass.
7. Unsupported or unverified combinations are named as such in
   [status](status.md); nothing is inferred from another platform.

Real install/uninstall, native window, accessibility, Agent survival, relay/TLS
and physical-iPhone results are separate evidence entries. A successful package
build is not acceptance.

## Rollback

- **Bad artifact:** rebuild from the last known-good SHA; never hand-edit or
  reuse a failed artifact, checksum or tag.
- **Bad merge:** revert on a new branch, run CI, merge. `main` is never
  force-pushed.
- **Bad tag:** publish a new patch tag. Moving or deleting a tag requires owner
  approval and does not rewrite already-downloaded artifacts.
- **Metadata damage:** see [runbooks/incident.md](runbooks/incident.md). Restoring
  metadata never requires stopping an Agent.

## What is not claimed

- A distributable signature (Developer ID or App Store), notarization, stapling
  or Gatekeeper acceptance.
- An installable iOS app, TestFlight or App Store distribution.
- Auto-update, delta updates, a package repository or a hosted download page.
- Windows service installation, launchd/systemd unit installation, or a
  notarized helper.
- Cross-platform results inferred from one host, or a physical-device result
  inferred from a simulator.
- Any artifact produced from a dirty tree or from a SHA without green CI.

See [release runbook](runbooks/release.md),
[incident runbook](runbooks/incident.md), [requirements](requirements.md),
[v1 completion](goals/v1-complete.md),
[implementation plan](goals/mygo-implementation.md) and [status](status.md).
