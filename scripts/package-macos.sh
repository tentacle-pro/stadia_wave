#!/bin/sh
set +x
set -eu

die() { printf '%s\n' "$*" >&2; exit 1; }

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
identity=${WAVE_SIGN_IDENTITY:-${APPLE_SIGNING_IDENTITY:--}}
notarize=${WAVE_NOTARIZE:-0}
stadiacast=${WAVE_STADIACAST_BINARY:-}
stadiacast_licenses=${WAVE_STADIACAST_LICENSE_DIR:-}
destination=${WAVE_PACKAGE_OUTPUT:-"$project_dir/target/macos"}

[ "$(uname -s)" = Darwin ] || die 'This script requires macOS.'
case "$notarize" in 0|1) ;; *) die 'WAVE_NOTARIZE must be 0 or 1.' ;; esac

if [ "$notarize" = 1 ]; then
    [ "$identity" != "-" ] || die 'Notarization requires a Developer ID Application identity.'
    security find-identity -v -p codesigning | awk -v identity="$identity" '
        /"Developer ID Application: / {
            name = $0
            sub(/^[^"]*"/, "", name)
            sub(/"$/, "", name)
            if ($2 == identity || name == identity) found = 1
        }
        END { exit !found }
    ' || die 'The specified Developer ID Application identity was not found in the Keychain.'

    if [ -n "${APPLE_NOTARY_PROFILE:-}" ]; then
        set -- --keychain-profile "$APPLE_NOTARY_PROFILE"
    elif [ -n "${APPLE_API_KEY:-}${APPLE_API_KEY_PATH:-}${APPLE_API_ISSUER:-}" ]; then
        : "${APPLE_API_KEY:?Set the App Store Connect API key ID}"
        : "${APPLE_API_KEY_PATH:?Set the App Store Connect API private key path}"
        : "${APPLE_API_ISSUER:?Set the App Store Connect API issuer ID}"
        [ -r "$APPLE_API_KEY_PATH" ] || die 'The App Store Connect API private key is unreadable.'
        set -- --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER"
    else
        : "${APPLE_ID:?Set the Apple account or supply APPLE_NOTARY_PROFILE}"
        : "${APPLE_PASSWORD:?Set the app-specific password}"
        : "${APPLE_TEAM_ID:?Set the Apple Developer team ID}"
        set -- --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID"
    fi
fi

if [ -n "$stadiacast" ]; then
    [ -f "$stadiacast" ] && [ -x "$stadiacast" ] || die 'WAVE_STADIACAST_BINARY must be an executable file.'
    [ -n "$stadiacast_licenses" ] && [ -f "$stadiacast_licenses/LICENSE" ] &&
        [ -f "$stadiacast_licenses/NOTICE" ] || die 'WAVE_STADIACAST_LICENSE_DIR must contain LICENSE and NOTICE.'
    if [ "$notarize" = 1 ]; then
        [ -d "$stadiacast_licenses/THIRD-PARTY-LICENSES" ] ||
            die 'Include the notices for dependencies bundled inside Stadiacast in THIRD-PARTY-LICENSES.'
        codesign --verify --strict "$stadiacast"
        signature=$(codesign --display --verbose=4 "$stadiacast" 2>&1)
        printf '%s\n' "$signature" | grep -q '^Authority=Developer ID Application:' &&
            printf '%s\n' "$signature" | grep -Eq '^CodeDirectory .*flags=.*\(runtime([,)]|$)' &&
            printf '%s\n' "$signature" | grep -q '^Timestamp=' ||
            die 'Use a Stadiacast build already signed with Developer ID, hardened runtime and a secure timestamp.'
    fi
elif [ -n "$stadiacast_licenses" ]; then
    die 'Set WAVE_STADIACAST_BINARY to include Stadiacast with its notices.'
fi

sign() {
    if [ "$notarize" = 1 ]; then
        codesign --force --options runtime --timestamp --sign "$identity" "$@"
    else
        codesign --force --timestamp=none --sign "$identity" "$@"
    fi
}

target_dir="$project_dir/target"
rust_target=$(rustc -vV | awk '/^host:/ { print $2 }')
env -u CARGO_BUILD_TARGET cargo build --release --locked --target-dir "$target_dir" -p wave-bridge -p wave-companion

mkdir -p "$destination"
stage=$(mktemp -d "$destination/.package.XXXXXXXX")
published_app="$destination/Wave Companion.app"
cleanup() {
    if [ -e "$stage/previous.app" ] && [ ! -e "$published_app" ]; then
        mv "$stage/previous.app" "$published_app" || {
            printf 'Previous app retained at %s\n' "$stage/previous.app" >&2
            return
        }
    fi
    rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
bundle="$stage/Wave Companion.app"
contents="$bundle/Contents"
licenses="$contents/Resources/Licenses"
mkdir -p "$contents/MacOS" "$contents/Helpers" "$licenses"
install -m 755 "$target_dir/release/wave-companion" "$contents/MacOS/wave-companion"
install -m 755 "$target_dir/release/wave-bridge" "$contents/Helpers/wave-bridge"
cp apps/companion/assets/StadiaWave.icns "$contents/Resources/"
cp scripts/companion-Info.plist "$contents/Info.plist"
cp LICENSE "$licenses/Stadia-Wave-LICENSE.txt"
rust_docs="$(rustc --print sysroot)/share/doc/rust"
[ -f "$rust_docs/COPYRIGHT-library.html" ] ||
    die 'Install the rust-docs component to include the Rust standard library notices.'
mkdir "$licenses/Rust-toolchain"
cp "$rust_docs/COPYRIGHT-library.html" "$licenses/Rust-toolchain/"
ditto "$rust_docs/licenses" "$licenses/Rust-toolchain/licenses"
cargo metadata --locked --format-version 1 --filter-platform "$rust_target" > "$stage/cargo-metadata.json"
version=$(python3 scripts/collect-macos-licenses.py "$licenses" < "$stage/cargo-metadata.json")
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"

if [ -n "$stadiacast" ]; then
    install -m 755 "$stadiacast" "$contents/Helpers/stadiacast"
    mkdir "$licenses/Stadiacast"
    cp "$stadiacast_licenses/LICENSE" "$stadiacast_licenses/NOTICE" "$licenses/Stadiacast/"
    if [ -d "$stadiacast_licenses/THIRD-PARTY-LICENSES" ]; then
        ditto "$stadiacast_licenses/THIRD-PARTY-LICENSES" "$licenses/Stadiacast/THIRD-PARTY-LICENSES"
    fi
    codesign --verify --strict "$contents/Helpers/stadiacast"
fi
sign --identifier dev.stadiawave.bridge "$contents/Helpers/wave-bridge"
sign "$bundle"
codesign --verify --deep --strict "$bundle"

archive="$stage/Wave-Companion-$version-$rust_target.zip"
ditto -c -k --keepParent "$bundle" "$archive"
if [ "$notarize" = 1 ]; then
    xcrun notarytool submit "$archive" "$@" --wait --output-format json > "$stage/notarization.json"
    result=$(plutil -extract status raw -o - "$stage/notarization.json")
    if [ "$result" != Accepted ]; then
        submission=$(plutil -extract id raw -o - "$stage/notarization.json")
        die "Notarization status: $result (submission $submission). Use notarytool log to inspect it."
    fi
    xcrun stapler staple "$bundle"
    xcrun stapler validate "$bundle"
    rm "$archive"
    ditto -c -k --keepParent "$bundle" "$archive"
fi

cp "$contents/Helpers/wave-bridge" "$stage/wave-bridge"
if [ -e "$published_app" ]; then
    mv "$published_app" "$stage/previous.app"
fi
mv "$bundle" "$published_app"
mv -f "$stage/wave-bridge" "$destination/wave-bridge"
mv -f "$archive" "$destination/"
printf '%s\n' "$destination"
