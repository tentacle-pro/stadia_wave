#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
WAVE_PACKAGE_ONLY=1 ./scripts/run-companion.sh
cargo build --release --locked -p wave-bridge -p wave-companion
destination="$project_dir/target/macos"
mkdir -p "$destination"
ditto "$project_dir/target/debug/Wave Companion.app" "$destination/Wave Companion.app"
cp target/release/wave-companion "$destination/Wave Companion.app/Contents/MacOS/wave-companion"
identity="${WAVE_SIGN_IDENTITY:-6491898B04D58BFDC11BE20B3A60C9FA23973B59}"
codesign --force --sign "$identity" "$destination/Wave Companion.app"
cp target/release/wave-bridge "$destination/wave-bridge"
codesign --force --identifier dev.stadiawave.bridge --sign "$identity" "$destination/wave-bridge"
codesign --verify --strict "$destination/Wave Companion.app"
codesign --verify --strict "$destination/wave-bridge"
echo "$destination"
