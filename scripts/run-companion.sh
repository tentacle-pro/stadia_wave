#!/bin/sh
# Builds the development Wave Companion.app and opens it.
#
# Signing is mandatory: an ad-hoc signature changes with every build, so macOS
# treats each build as a new app and asks for Accessibility and Local Network
# permission again. Set a stable identity first:
#
#   export APPLE_SIGNING_IDENTITY="Apple Development: Your Name (TEAMID)"
#   security find-identity -v -p codesigning   # lists the available identities
set -eu

die() { printf '%s\n' "$*" >&2; exit 1; }

identity=${APPLE_SIGNING_IDENTITY:-${WAVE_SIGN_IDENTITY:-}}
[ -n "$identity" ] || die 'Set APPLE_SIGNING_IDENTITY (or WAVE_SIGN_IDENTITY) to a code-signing identity first.
Ad-hoc signatures change on every build, so macOS asks for permissions again each time.
Example: export APPLE_SIGNING_IDENTITY="Apple Development: Your Name (TEAMID)"
List identities with: security find-identity -v -p codesigning'

security find-identity -v -p codesigning | awk -v identity="$identity" '
    /^[[:space:]]*[0-9]+\)/ {
        hash = $2
        name = $0
        sub(/^[^"]*"/, "", name)
        sub(/"$/, "", name)
        if (hash == identity || name == identity) found = 1
    }
    END { exit !found }
' || die "Signing identity not found in the Keychain: $identity"

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
cargo build -p wave-companion --bin wave-companion
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
