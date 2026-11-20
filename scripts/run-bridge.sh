#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
cargo build -p wave-bridge
log_dir="$HOME/Library/Application Support/dev.stadiawave.bridge/logs"
mkdir -p "$log_dir"
chmod 700 "$log_dir"
log_file="$log_dir/$(date +%Y%m%d-%H%M%S)-$$.log"
echo "Bridge log: $log_file"
exec ./target/debug/wave-bridge "$@" >>"$log_file" 2>&1
