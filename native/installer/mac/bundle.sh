#!/bin/sh
# Builds "Agu Chat.app" around a compiled agu-chat-gpui binary and zips it. macOS only (sips, iconutil, codesign, ditto).
#   bundle.sh <binary> <version> <out dir>
set -eu
bin="$1"; version="$2"; out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
app="$out/Agu Chat.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/Agu Chat"
sed "s/__VERSION__/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

# the icon: every size macOS asks for, from the 1024 px PNG
iconset="$out/AppIcon.iconset"
rm -rf "$iconset"; mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s "$here/../icon.png" --out "$iconset/icon_${s}x${s}.png" > /dev/null
  d=$((s * 2))
  sips -z $d $d "$here/../icon.png" --out "$iconset/icon_${s}x${s}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

# ad-hoc signature: required to run on Apple Silicon; not a Developer ID, so Gatekeeper still asks on first open
codesign --force --deep --sign - "$app"
ditto -c -k --keepParent "$app" "$out/Agu-Chat-$version-mac.zip"
echo "built $out/Agu-Chat-$version-mac.zip"
