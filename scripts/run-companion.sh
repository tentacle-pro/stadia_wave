#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
cargo build -p wave-companion --bin wave-companion
bundle="$project_dir/target/debug/Wave Companion.app"
mkdir -p "$bundle/Contents/MacOS"
cp "$project_dir/target/debug/wave-companion" "$bundle/Contents/MacOS/wave-companion"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>wave-companion</string>
<key>CFBundleIdentifier</key><string>dev.stadiawave.companion</string>
<key>CFBundleName</key><string>Wave Companion</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSLocalNetworkUsageDescription</key><string>发现并连接局域网内的 Wave Bridge，接收 Stadia 手柄输入。</string>
<key>NSBonjourServices</key><array><string>_stadia-wave._tcp</string></array>
</dict></plist>
PLIST
codesign --force --sign - "$bundle"
open "$bundle"
