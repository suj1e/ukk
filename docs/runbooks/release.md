# Release runbook

Operator steps for unsigned artifacts built from the Go tree. The packaging
contract is [docs/release.md](../release.md); this runbook is the executable
procedure. It never applies a distributable signature, notarizes, publishes a
GitHub Release or claims an install result that was not observed.

## 1. Preconditions

- CI green on the exact commit SHA in `.github/workflows/ci.yml`; iOS workflow
  green for the same SHA when the iOS project changed.
- `just check` passes locally (`layout`, `docs`, `versions`).
- Tools: Go `1.27.3` (module-pinned), MyGo `v0.8.3` via `go tool mygo`,
  XcodeGen for iOS, `makensis` for the Windows installer, and the Linux GTK
  development packages listed in [release.md](../release.md).
- The tag version equals `apps/desktop/mygo.json` `version` and the iOS
  `MARKETING_VERSION`; `python3 scripts/check_versions.py` proves it.

## 2. Build and static checks

```sh
cd apps/desktop
gofmt -l .                 # must print nothing
go vet ./...
go test ./...
go test -race ./...        # on supported hosts; record unsupported platforms
go tool mygo build -platform darwin/universal -o ../../dist/desktop
go tool mygo build -platform windows/amd64    -o ../../dist/desktop
go tool mygo build -platform linux/amd64      -o ../../dist/desktop
go build -trimpath -o ../../dist/desktop/linux/amd64/ukk-relay  ./cmd/ukk-relay
go build -trimpath -o ../../dist/desktop/linux/amd64/ukk-bridge ./cmd/ukk-bridge
```

Notes:

- The CI matrix builds each target on its native runner. Building all three
  locally is optional and depends on the cross-toolchain and `makensis`; a
  local success is not evidence for another OS.
- The macOS `.dmg` is produced only when packaging runs on macOS; the other
  targets still need `makensis` for the Windows setup program.
- `cmd/ukk` and `cmd/ukk-bridge` must exist for a desktop package. A build that
  cannot produce them is a blocked release, not a reduced one.
- Build the bridge/relay for each target that ships them; do not copy a Linux
  binary into a macOS bundle.
- Never package from a dirty tree. `git status --short` must be empty apart from
  ignored `dist/`.

## 3. Verify artifacts

Output lives under `dist/desktop/<os>/<arch>/`.

macOS:

```sh
hdiutil attach -nobrowse -readonly dist/desktop/darwin/universal/*.dmg
APP="$(find /Volumes -maxdepth 2 -name 'ukk.app' -type d -print -quit)"
plutil -lint "$APP/Contents/Info.plist"
codesign -dvv "$APP" 2>&1 | grep -E 'Identifier|Signature|TeamIdentifier'
spctl --assess --type execute --verbose "$APP"   # expected to reject
open "$APP"                                      # record whether it launches
hdiutil detach "$(dirname "$APP")" || true
```

`Signature=adhoc` and a `spctl` rejection are the expected unsigned state. A
locally built bundle may launch without a quarantine flag, while a downloaded
copy is normally blocked; record which one was observed. Do not work around
Gatekeeper in release notes and do not record a block as a failure of
packaging. Launch the bundle executable directly only to observe a native
window, and record that as a separate real-window result.

Windows (on a disposable VM or clean profile):

```sh
dist/desktop/windows/amd64/ukk-setup-*.exe
```

Walk the installer, confirm the Start Menu entry and uninstaller, launch the
app, close it, then uninstall. Silent flags (`/S`) are not acceptance evidence.
Record the OS build and what was actually clicked.

Linux:

```sh
dpkg-deb -I dist/desktop/linux/amd64/*.deb
dpkg-deb -x dist/desktop/linux/amd64/*.deb /tmp/ukk-deb
tar -tzf dist/desktop/linux/amd64/*.tar.gz | head
sudo dpkg -i dist/desktop/linux/amd64/*.deb     # disposable VM only
```

Confirm the `.desktop` entry, the binary launch and uninstall
(`sudo dpkg -r ukk`). A `.deb` that installs but does not launch is not a pass.

iOS (unsigned device build, artifact only):

```sh
cd apps/ios
xcodegen generate
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath "$PWD/build/UkkCompanion.xcarchive" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY="" archive
mkdir -p build/ipa/Payload
cp -R build/UkkCompanion.xcarchive/Products/Applications/UkkCompanion.app build/ipa/Payload/
(cd build/ipa && zip -qry ../ukk-unsigned.ipa Payload)
shasum -a 256 build/ukk-unsigned.ipa > build/ukk-unsigned.ipa.sha256
```

This IPA cannot be installed: it has no signature and no provisioning profile.
Do not send it to a device test plan and do not describe a simulator run as
camera, APNs or physical-device evidence.

Release set: confirm `ukk-bridge` (and `ukk-relay` for operator distributions)
is present at the expected location. Check a version flag where the entrypoint
implements one; today that is the relay:

```sh
./dist/desktop/linux/amd64/ukk-relay -version   # prints ukk-relay <version>
```

A missing bridge binary blocks the release; so does a version flag that starts
a listener, service or window. A binary without any version flag is a known gap
to fix, not a pass.

## 4. Checksums

The workflow writes `SHA256SUMS.txt` at the artifact root (`dist/desktop`).
Verify a fresh copy before trusting it:

```sh
cd dist/desktop
shasum -a 256 -c SHA256SUMS.txt
```

If you must regenerate (for example after adding the bridge binary), run the
same command from `dist/desktop` and keep the format the workflow uses — sorted
paths, two spaces, LF endings only:

```sh
python3 - <<'PY'
import hashlib, pathlib
root = pathlib.Path('.')
lines = []
for path in sorted(p for p in root.rglob('*') if p.is_file() and p.name != 'SHA256SUMS.txt'):
    lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.as_posix()}")
pathlib.Path('SHA256SUMS.txt').write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
PY
```

A CRLF checksum file is invalid: `sha256sum -c` will fail on Linux. Prove the
line endings before shipping:

```sh
python3 -c 'import sys; sys.exit(0 if b"\r" not in open("SHA256SUMS.txt","rb").read() else 1)' \
  && echo "LF-only checksums"
```

## 5. Deploy the relay

The relay is loopback by default and expects TLS in front of it or direct TLS
certificates.

```sh
# Build and place
cd apps/desktop
go build -trimpath -o /usr/local/bin/ukk-relay ./cmd/ukk-relay

# Operator token: at least 32 bytes, 0600, owned by the relay user
sudo useradd --system --home /nonexistent --shell /usr/sbin/nologin ukk 2>/dev/null || true
sudo install -d -m 0700 -o ukk -g ukk /etc/ukk
openssl rand -hex 32 | sudo tee /etc/ukk/relay.token >/dev/null
sudo chown ukk:ukk /etc/ukk/relay.token && sudo chmod 0600 /etc/ukk/relay.token
```

Run behind a TLS reverse proxy (recommended):

```sh
ukk-relay -addr 127.0.0.1:8787 -token-file /etc/ukk/relay.token
```

Caddy example (WebSocket upgrades are automatic):

```caddyfile
relay.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

nginx example:

```nginx
location / {
    proxy_pass http://127.0.0.1:8787;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_read_timeout 300s;
}
```

Direct TLS is also supported; both flags are required together:

```sh
ukk-relay -addr 0.0.0.0:8787 \
  -token-file /etc/ukk/relay.token \
  -tls-cert /etc/ukk/fullchain.pem \
  -tls-key /etc/ukk/privkey.pem
```

A minimal unit for the reverse-proxy deployment:

```ini
# /etc/systemd/system/ukk-relay.service
[Unit]
Description=ukk control-plane relay
After=network-online.target

[Service]
User=ukk
Group=ukk
ExecStart=/usr/local/bin/ukk-relay -addr 127.0.0.1:8787 -token-file /etc/ukk/relay.token
Restart=on-failure
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now ukk-relay
journalctl -u ukk-relay -n 50 --no-pager
```

Checks:

```sh
curl -fsS http://127.0.0.1:8787/healthz            # prints: ok
curl -fsS https://relay.example.com/healthz        # through the proxy
curl -si -H 'Authorization: Bearer wrong' https://relay.example.com/host | head -1
# expect 401; a real host needs a WSS upgrade and a hello frame
```

Firewall only `443` (or the chosen TLS port). Never expose `8787` publicly
without TLS. Host and device WebSocket paths are `/host`
(`Authorization: Bearer <operator token>`) and `/device`; after the upgrade the
first frame is `{"t":"hello",…}`.

Configure the bridge on the host machine in `<data dir>/bridge.json` (mode
0600):

```json
{
  "host_id": "…",
  "relay_url": "wss://relay.example.com",
  "operator_token": "…",
  "stun_urls": ["stun:turn.example.com:3478"],
  "turn": {
    "urls": ["turn:turn.example.com:3478?transport=udp"],
    "username": "ukk",
    "credential": "…"
  }
}
```

`relay_url` must be `wss://` (the bridge only permits `ws://127.0.0.1` for local
tests). Restart the bridge and confirm the relay reports `host_connected`;
confirm the paired device connects from the bridge and app state rather than
assuming a relay log line. Relay lifecycle events are routing metadata only
(`host_connected`, `host_disconnected`, `device_connected`,
`device_disconnected`, `conn_opened`, `conn_closed`, `device_revoked`,
`device_repaired`, `rejected`, `queue_overflow`). A relay restart forgets its
deny set; durable revocation remains the bridge's job, so re-test a revoked
device after restarting the relay.

## 6. TURN and media

- TURN is independent of the relay; the relay must never carry screen media.
- Deploy coturn on a host with public UDP reachability; restrict
  `min-port`/`max-port`, set a realm and credentials, and use short-lived
  credentials where the deployment supports them.
- Put the STUN/TURN endpoints and credentials in `bridge.json` only. The file is
  `0600`; credentials are never logged or committed.
- Verify with a phone on cellular: the screen session negotiates ICE candidates
  and media flows. Check relay egress: no video-sized or continuous stream from
  the media path. A TURN relay connection is expected; a media stream over the
  relay WebSocket is a defect.

## 7. Tag and CI artifacts

```sh
git switch main && git pull --ff-only
git tag v0.1.0
git push origin v0.1.0
gh run list --workflow 'Release (unsigned)' --limit 1
gh run watch <run-id>
gh run download <run-id> -D dist/ci
```

Then repeat the checksum and package checks above on the downloaded artifacts.
A tag run produces the same unsigned artifacts and does not create a Release.
Record the run URL, SHA, artifact hashes and OS versions in the PR or
[status.md](../status.md).

## 8. Rollback

- **Bad artifact:** rebuild from the previous known-good SHA. Do not reuse or
  hand-patch an artifact, checksum or tag.
- **Bad merge:** revert on a new branch, run CI, merge. Never force-push `main`.
- **Bad tag:** publish a new patch tag. Moving or deleting an existing tag
  requires owner approval and never rewrites downloaded artifacts.
- **Relay regression:** stop the relay, keep the bridge and Agent processes
  running, redeploy the previous relay binary, then confirm host and device
  reconnect. Relay downtime must not stop Agents.
- **Metadata damage:** follow [incident.md](incident.md); restore metadata
  without stopping terminals.

## 9. Stop conditions

Do not publish when any of these is true:

- CI is red or absent for the exact SHA, or the tree was dirty.
- A platform artifact, the bridge binary or the checksum file is missing.
- Checksums are CRLF, unsorted or fail verification.
- The tag, `VERSION.txt`, `mygo.json` version and reported binary version
  disagree.
- The macOS bundle claims a signature, notarization or Gatekeeper acceptance.
- The unsigned IPA is described as installable.
- The packaging command used `-upload`, or the app configures an update feed, so
  the build attempts to publish.
- A platform or device result is copied from another platform.
