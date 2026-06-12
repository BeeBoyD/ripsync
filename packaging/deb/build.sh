#!/usr/bin/env bash
# Build a ripsync .deb with cargo-deb.
#
# Prereqs: `cargo install cargo-deb`. Run from anywhere; paths resolve to the
# workspace root. The .deb lands in target/debian/.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

echo ">> building release binary"
cargo build --release -p ripsync

echo ">> generating man page + shell completions"
cargo run --release -p xtask -- dist-assets

echo ">> packaging .deb"
# --no-build: reuse the release binary we just built above.
cargo deb -p ripsync --no-build

echo ">> done:"
ls -1 target/debian/*.deb
