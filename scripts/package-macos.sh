#!/usr/bin/env bash
# Builds a minimal, ad-hoc signed Flipperaki.app so macOS grants Bluetooth
# (TCC) to a stable identity rather than to whichever terminal ran the binary.
# Output: dist/Flipperaki.app and dist/SHA256SUMS. Does not install anything.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked
app=dist/Flipperaki.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/flipperaki "$app/Contents/MacOS/flipperaki"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>io.github.prokolyvakis.flipperaki</string>
  <key>CFBundleName</key><string>Flipperaki</string>
  <key>CFBundleExecutable</key><string>flipperaki</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSUIElement</key><true/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSBluetoothAlwaysUsageDescription</key>
  <string>Flipperaki connects to your Flipper Zero over Bluetooth to show Claude's status and, with your approval on the device, operate its apps.</string>
</dict>
</plist>
PLIST
# Ad-hoc signature: a stable local identity for TCC, not a distributable release.
codesign --force --sign - --identifier io.github.prokolyvakis.flipperaki "$app"
codesign --verify --strict "$app"
(cd dist && shasum -a 256 Flipperaki.app/Contents/MacOS/flipperaki > SHA256SUMS)
if [ -f flipper/dist/flipperaki.fap ]; then
    shasum -a 256 flipper/dist/flipperaki.fap | sed 's| flipper/dist/| |' >> dist/SHA256SUMS
fi
cat dist/SHA256SUMS
