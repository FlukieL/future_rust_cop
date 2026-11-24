#!/bin/sh
# Builds fcop.app and packs it into one file to share: target/fcop-<version>-macos.zip.
# The app keeps the game files in ~/Library/Application Support/fcop (setup::app_folder).
set -e
cd "$(dirname "$0")/.."
cargo build --release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
app=target/fcop.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/fcop "$app/Contents/MacOS/fcop"
# the icon: drawn by examples/icon.rs, every size macOS asks for
icons=target/fcop.iconset
rm -rf "$icons"
mkdir -p "$icons" "$app/Contents/Resources"
cargo run -q --release --example icon -- target/icon.png
for s in 16 32 128 256 512; do
    sips -z $s $s target/icon.png --out "$icons/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) target/icon.png --out "$icons/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$icons" -o "$app/Contents/Resources/fcop.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>fcop</string>
    <key>CFBundleDisplayName</key><string>fcop</string>
    <key>CFBundleExecutable</key><string>fcop</string>
    <key>CFBundleIconFile</key><string>fcop</string>
    <key>CFBundleIdentifier</key><string>io.github.future-rust-cop.fcop</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.action-games</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
# no developer certificate: an ad-hoc signature, which seals Info.plist with the program
codesign --force --sign - "$app"
zip="target/fcop-$version-macos.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "$zip"
