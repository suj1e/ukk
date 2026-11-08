#!/usr/bin/env bash
# Download the checksum-pinned LiveKit WebRTC xcframework used by the iOS
# screen viewer. GitHub's release CDN intermittently returns 504; retry with
# both the GitHub CLI and curl. The archive is not committed.
set -euo pipefail

version="150.7871.05"
archive="LiveKitWebRTC.xcframework.zip"
# SHA-256 declared by livekit/webrtc-xcframework 150.7871.05/Package.swift.
checksum="b24d7df29733079038931698102fbb39cbc11b014e4352beb0d4f55a01c3c37e"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/ThirdParty/LiveKitWebRTC"
target="$dest/LiveKitWebRTC.xcframework"

if [ -d "$target" ]; then
  echo "LiveKitWebRTC.xcframework already present"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
url="https://github.com/livekit/webrtc-xcframework/releases/download/${version}/${archive}"

for attempt in 1 2 3 4 5; do
  echo "fetching ${archive} (attempt ${attempt})"
  if command -v gh >/dev/null 2>&1 && \
     gh release download "$version" -R livekit/webrtc-xcframework -p "$archive" -D "$tmp" --clobber; then
    break
  fi
  if curl -fL --retry 5 --retry-all-errors --connect-timeout 20 -o "$tmp/$archive" "$url"; then
    break
  fi
  sleep $((attempt * 5))
done

if [ ! -s "$tmp/$archive" ]; then
  echo "could not download ${archive}" >&2
  exit 1
fi

if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
else
  actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
fi
if [ "$actual" != "$checksum" ]; then
  echo "checksum mismatch: got ${actual}, want ${checksum}" >&2
  exit 1
fi

mkdir -p "$tmp/unzipped"
unzip -q "$tmp/$archive" -d "$tmp/unzipped"
if [ ! -d "$tmp/unzipped/LiveKitWebRTC.xcframework" ]; then
  echo "archive did not contain LiveKitWebRTC.xcframework" >&2
  exit 1
fi
rm -rf "$target"
mv "$tmp/unzipped/LiveKitWebRTC.xcframework" "$target"
echo "installed $target"
