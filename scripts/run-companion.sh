#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
cargo build -p wave-companion --bin wave-companion
identity="${WAVE_SIGN_IDENTITY:-6491898B04D58BFDC11BE20B3A60C9FA23973B59}"
bundle="$project_dir/target/debug/Wave Companion.app"
icon_source="$project_dir/apps/companion/assets/StadiaWave.icns"
icon_destination="$bundle/Contents/Resources/StadiaWave.icns"
plist_source="$project_dir/scripts/companion-Info.plist"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
if [ -f "$bundle/Contents/MacOS/wave-companion" ] &&
   [ "$(shasum -a 256 "$project_dir/target/debug/wave-companion" | cut -d ' ' -f1)" = "$(cat "$project_dir/target/debug/companion-build-hash" 2>/dev/null || true)" ] &&
   [ "$(cat "$project_dir/target/debug/companion-signing-identity" 2>/dev/null || true)" = "$identity" ] &&
   cmp -s "$icon_source" "$icon_destination" &&
   cmp -s "$plist_source" "$bundle/Contents/Info.plist" &&
   codesign --verify --strict "$bundle" 2>/dev/null; then
    [ "${WAVE_PACKAGE_ONLY:-0}" = 1 ] || open "$bundle"
    exit 0
fi
cp "$project_dir/target/debug/wave-companion" "$bundle/Contents/MacOS/wave-companion"
cp "$icon_source" "$icon_destination"
cp "$plist_source" "$bundle/Contents/Info.plist"
codesign --force --sign "$identity" "$bundle"
shasum -a 256 "$project_dir/target/debug/wave-companion" | cut -d ' ' -f1 > "$project_dir/target/debug/companion-build-hash"
printf '%s\n' "$identity" > "$project_dir/target/debug/companion-signing-identity"
[ "${WAVE_PACKAGE_ONLY:-0}" = 1 ] || open "$bundle"
