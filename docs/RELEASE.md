# Release artifacts / 发布产物

> 2026-11-08 · **Unsigned development artifacts, not a shipped release.** This
> document describes what the packaging workflow actually builds and verifies.
> It does not claim signing, notarization, installers, auto-update, App Store
> distribution or a Windows service.

The `Unsigned release artifacts` workflow
(`.github/workflows/release-artifacts.yml`) builds `ukk-host` and `ukk-desktop`
in release mode on macOS, Windows and Linux, packages one archive per OS and
uploads it as a GitHub Actions artifact. Every push to `main`, every `v*` tag
and manual dispatch produces downloadable unsigned packages; pull requests run
it only when packaging inputs change. It never creates a GitHub Release and
never uploads to an external service.

## What is in an archive

Every archive has a single top-level directory named
`ukk-<version>-<os>-<arch>`:

```text
ukk-<version>-<os>-<arch>/
├── LICENSE
├── README.md
├── SHA256SUMS.txt
├── VERSION.txt
├── ukk-host          # ukk-host.exe on Windows
└── ukk-desktop       # ukk-desktop.exe on Windows
```

| OS | Archive | Executables |
|---|---|---|
| macOS | `ukk-<version>-macos-<arch>.tar.gz` | `ukk-host`, `ukk-desktop` |
| Linux | `ukk-<version>-linux-<arch>.tar.gz` | `ukk-host`, `ukk-desktop` |
| Windows | `ukk-<version>-windows-<arch>.zip` | `ukk-host.exe`, `ukk-desktop.exe` |

`VERSION.txt` records the embedded version, source commit, Rust target triple,
OS, architecture and both binary names. `SHA256SUMS.txt` covers every file in
the archive except itself, in standard `<sha256>  <file>` form. The workflow
also writes `<archive>.sha256` next to each archive so the downloaded file can
be checked before extraction.

## Build identity and `--version`

Both binaries embed build identity at compile time. `build.rs` reads
`UKK_BUILD_VERSION` and `UKK_BUILD_COMMIT`; when unset it falls back to the
crate version and `unknown`.

```console
$ ukk-host --version
ukk-host version=0.1.0-dev.abc1234 commit=<40-char sha> profile=release target=macos/aarch64

$ ukk-desktop --version
ukk-desktop version=0.1.0-dev.abc1234 commit=<40-char sha> profile=release target=macos/aarch64
```

`--version`, `-V` and `version` are accepted. `ukk-host version` also appears in
`ukk-host help`. These commands print and exit; they do not start the Host, open
a window or connect to herdr-core.

Version resolution in CI:

| Trigger | Version string |
|---|---|
| Tag `v1.2.3` | `1.2.3` |
| `main` push, `workflow_dispatch` or pull request | `<package version>-dev.<first 7 sha chars>` |

The workflow reads the package version from `crates/host/Cargo.toml`, derives
the Rust host triple with `rustc -vV`, and uses the runner's actual OS/arch.
The archives are **not** universal macOS binaries and **not** cross-compiled.

## CI coverage

The matrix is `macos-latest`, `windows-latest` and `ubuntu-latest`. Each job:

1. installs Rust stable and, on Linux, the GPUI build dependencies (the same
   list used by `desktop-cross-platform.yml`);
2. builds `ukk-host` and `ukk-desktop` in release mode;
3. runs both `--version` commands and fails if they exit non-zero;
4. packages the archive with `scripts/package_release.py`;
5. re-opens the archive, verifies every member is non-empty and re-checks the
   internal `SHA256SUMS.txt` before upload;
6. uploads the archive and `.sha256` file as a workflow artifact.

`scripts/package_release.py` uses only the Python standard library and works on
all three runners. Archive metadata is fixed (sorted entries, tar mtime from
`SOURCE_DATE_EPOCH` or 0, zip timestamps 1980-01-01), so packaging the same
binaries and identity twice produces the same archive bytes.

## Build locally

Host (all platforms, no GPU toolchain required):

```bash
export UKK_BUILD_VERSION="0.1.0-local"
export UKK_BUILD_COMMIT="$(git rev-parse HEAD)"
cargo build --release -p ukk-host
./target/release/ukk-host --version
```

Desktop:

```bash
cargo build --release --manifest-path apps/desktop/Cargo.toml --bin ukk-desktop
./apps/desktop/target/release/ukk-desktop --version
```

- **macOS:** the default GPUI build compiles Metal shaders at build time with
  `xcrun metal` and `xcrun metallib`. A full Xcode installation with the Metal
  Toolchain component is required (`xcodebuild -downloadComponent
  MetalToolchain`). Without it the Desktop build fails while the Host still
  builds. GitHub's macOS runners include the toolchain.
- **Linux:** install the same packages the workflow installs, plus a working
  Rust stable toolchain. Release binaries are dynamically linked against the
  system libraries used at build time.
- **Windows:** Rust stable with the MSVC toolchain and Visual Studio C++ build
  tools. No signing certificate is used.

Do **not** enable `gpui_apple`'s `runtime_shaders` feature for these artifacts.
It is an escape hatch for CI-only smoke checks when the Metal toolchain is
unavailable; binaries built that way are not release claims and must not be
published or promoted.

Package locally:

```bash
python3 scripts/package_release.py \
  --os macos --arch aarch64 \
  --version "$UKK_BUILD_VERSION" --commit "$UKK_BUILD_COMMIT" \
  --out-dir dist
```

The script rejects missing/empty binaries, unexpected binary names, unsafe
version strings and incomplete archives.

## Verify a downloaded archive

macOS:

```bash
shasum -a 256 -c ukk-<version>-macos-<arch>.tar.gz.sha256
tar -xzf ukk-<version>-macos-<arch>.tar.gz
cd ukk-<version>-macos-<arch>
shasum -a 256 -c SHA256SUMS.txt
```

Linux:

```bash
sha256sum -c ukk-<version>-linux-<arch>.tar.gz.sha256
tar -xzf ukk-<version>-linux-<arch>.tar.gz
cd ukk-<version>-linux-<arch>
sha256sum -c SHA256SUMS.txt
```

Windows PowerShell (after extracting the zip):

```powershell
Get-Content .\SHA256SUMS.txt | ForEach-Object {
  $hash, $file = $_ -split '  ', 2
  if ((Get-FileHash -Algorithm SHA256 $file).Hash -ne $hash.ToUpperInvariant()) {
    throw "checksum mismatch: $file"
  }
}
```

## Run and recover the Host

`ukk-host` is the local Agent Host; `ukk-desktop` is the GPUI client that reads
it. Neither binary replaces `herdr-core` or `hc`: install those separately and
make `hc` available on `PATH` or via `UKK_HC_BIN`.

```bash
# Foreground Host on a private Unix socket (macOS/Linux) or Named Pipe (Windows)
./ukk-host serve

# In another terminal:
./ukk-host request terminal.list
./ukk-host status
```

- Database: `UKK_DB_PATH` if set; otherwise
  `~/.ukk/data/workspace.sqlite3` on macOS/Linux and
  `%LOCALAPPDATA%\ukk\workspace.sqlite3` on Windows. Back up the file while the
  Host is stopped; migrations run on open.
- Unix socket: `UKK_SOCKET` if set; otherwise the Host's default private socket
  directory. `ukk-host cleanup` removes a stale socket only after proving no
  live Host owns it.
- Recovery: after a Host crash, run `ukk-host status`, then
  `ukk-host cleanup` if the socket is stale. `ukk-host restart` restarts only
  the Host. None of these commands send stop/kill signals to `herdr-core` or its
  terminals.
- Closing the Desktop window does not stop the Host or any Agent. If the Host is
  not running, the Desktop shows an offline state rather than inventing data.
- `ukk-host install|uninstall|status|cleanup` manage a **current-user** service
  artifact (launchd user agent, systemd `--user` unit, or a Windows per-user
  Scheduled Task). These commands exist in the binary but this workflow neither
  runs nor verifies service registration. There is no LocalSystem service and
  no machine-wide install.

## What these artifacts do and do not guarantee

Verified by this workflow when it is green:

- Both binaries compile in release mode from the same commit on macOS, Windows
  and Linux runners.
- Both binaries run their `--version` path and report the embedded version,
  commit and target.
- Each archive is non-empty, contains the expected file set, and its internal
  `SHA256SUMS.txt` matches the archived bytes.
- Artifacts are uploaded only to the workflow run's artifact storage.

Explicitly **NOT guaranteed / NOT VERIFIED**:

- **No code signing.** macOS Gatekeeper and Windows SmartScreen will warn.
- **No notarization.** macOS artifacts are not notarized or stapled.
- **No installer.** There is no `.dmg`, `.pkg`, `.msi`, `.deb`, `.rpm`,
  AppImage or setup wizard.
- **No auto-updater.** Nothing checks for or downloads updates.
- **No Windows service registration.** The Windows Host uses a current-user
  Scheduled Task when explicitly installed; this workflow does not install or
  validate it, and no Windows *service* is created.
- **No App Store / TestFlight distribution.** iOS is out of scope for this
  workflow.
- **No launch smoke on Desktop.** The `--version` path is exercised, but no
  native window is opened in this workflow. Real GPUI window evidence remains
  separate (`desktop-launch-smoke.yml`) and is currently NOT VERIFIED on
  Windows.
- **No real `herdr-core` integration.** These jobs do not install or run `hc`,
  so Agent launch/attach/survival is not proven by this workflow.
- **No dependency lockfile.** The repository does not commit `Cargo.lock`, so
  binary reproducibility across time is not promised; only archive packaging of
  a given set of binaries is deterministic.
- **No cross-user or permission-denial validation** of the Host endpoint.
- **Not a supported/production release.** Version strings containing `dev.` are
  development builds; treat every artifact as unsigned developer tooling.

## Support matrix

| OS | CI runner | Rust target built by CI | Archive | Signing |
|---|---|---|---|---|
| macOS | `macos-latest` | `aarch64-apple-darwin` (arm64 runner) | `tar.gz` | none |
| Linux | `ubuntu-latest` | `x86_64-unknown-linux-gnu` | `tar.gz` | none |
| Windows | `windows-latest` | `x86_64-pc-windows-msvc` | `zip` | none |

The exact triple is taken from `rustc -vV` in each run and is recorded in
`VERSION.txt`. Intel macOS and Windows arm64 are not built by this matrix; the
same source is expected to compile there but those artifacts are NOT VERIFIED.

Related: [Platform support](PLATFORM_SUPPORT.md), [Host lifecycle](HOST_LIFECYCLE.md),
[Desktop spike](DESKTOP_SPIKE.md), [Engineering standards](ENGINEERING.md).
