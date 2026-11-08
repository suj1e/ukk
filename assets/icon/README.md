# ukk app icon

Concept: **split workspace, one active automation pane**. Three rounded panes
describe the multi-pane workspace; the lavender pane carries a thick open loop
and blunt arrowhead for continuous Agent execution. Ink `#242730`,
off-white `#F8F9FB`, lavender `#6E74E8`.

## Source of truth

- `ukk-square.svg` — full-bleed square for iOS (the OS masks corners).
- `ukk-macos.svg` — transparent canvas with the rounded tile for macOS,
  Windows and Linux packaging.
- `ukk.icns` — macOS bundle icon.
- `ukk.ico` — Windows installer/application icon.
- `ukk-square-1024.png` — iOS `AppIcon` source.
- `ukk-512.png` — Linux icon.

## Regenerate

```bash
rsvg-convert -w 1024 -h 1024 ukk-square.svg -o ukk-square-1024.png
rsvg-convert -w 1024 -h 1024 ukk-macos.svg -o ukk-macos-1024.png

rm -rf /tmp/ukk.iconset && mkdir /tmp/ukk.iconset
for s in 16 32 64 128 256 512 1024; do
  sips -z "$s" "$s" ukk-macos-1024.png --out "/tmp/ukk.iconset/icon_${s}.png" >/dev/null
done
# Create the standard @1x/@2x names, then:
iconutil -c icns /tmp/ukk.iconset -o ukk.icns

python3 - <<'PY'
from PIL import Image
img = Image.open("ukk-macos-1024.png")
img.save("ukk.ico", sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])
img.resize((512, 512), Image.LANCZOS).save("ukk-512.png")
PY

cp ukk-square-1024.png ../../apps/ios/UkkCompanion/Resources/Assets.xcassets/AppIcon.appiconset/AppIcon.png
```

Do not apply a macOS-style mask to the iOS square; Xcode applies the iOS mask.
The macOS archive stages `ukk.icns` at `ukk.app/Contents/Resources/ukk.icns`.
