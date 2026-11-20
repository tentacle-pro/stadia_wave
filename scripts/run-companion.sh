#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
cargo build -p wave-companion --bin wave-companion
identity="${WAVE_SIGN_IDENTITY:-6491898B04D58BFDC11BE20B3A60C9FA23973B59}"
bundle="$project_dir/target/debug/Wave Companion.app"
mkdir -p "$bundle/Contents/MacOS"
if [ -f "$bundle/Contents/MacOS/wave-companion" ] &&
   [ "$(shasum -a 256 "$project_dir/target/debug/wave-companion" | cut -d ' ' -f1)" = "$(cat "$project_dir/target/debug/companion-build-hash" 2>/dev/null || true)" ] &&
   [ "$(cat "$project_dir/target/debug/companion-signing-identity" 2>/dev/null || true)" = "$identity" ] &&
   codesign --verify --strict "$bundle" 2>/dev/null; then
    [ "${WAVE_PACKAGE_ONLY:-0}" = 1 ] || open "$bundle"
    exit 0
fi
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
codesign --force --sign "$identity" "$bundle"
shasum -a 256 "$project_dir/target/debug/wave-companion" | cut -d ' ' -f1 > "$project_dir/target/debug/companion-build-hash"
printf '%s\n' "$identity" > "$project_dir/target/debug/companion-signing-identity"
[ "${WAVE_PACKAGE_ONLY:-0}" = 1 ] || open "$bundle"
