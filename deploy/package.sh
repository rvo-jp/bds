#!/bin/sh
# Run on a Linux build machine after cargo build --release --locked.
set -eu
cd "$(dirname "$0")/.."
artifact_dir=${1:-dist}
binary=${2:-target/release/bdsctl}
mkdir -p "$artifact_dir"
install -m 0755 "$binary" "$artifact_dir/bdsctl-linux-x86_64"
sha256sum Cargo.toml Cargo.lock build.rs src/*.rs > "$artifact_dir/SOURCES.sha256"
(cd "$artifact_dir" && sha256sum bdsctl-linux-x86_64 SOURCES.sha256 > SHA256SUMS)
