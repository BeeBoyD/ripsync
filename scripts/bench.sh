#!/usr/bin/env bash
# Thin wrapper around the cross-platform harness in bench.py. Builds the release
# binary, then runs the Python benchmark. All configuration is via environment
# variables documented at the top of bench.py (RUNS, CACHE_MODE,
# BENCH_SCENARIOS, dataset sizing, …).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --release --manifest-path "$ROOT/Cargo.toml" >/dev/null
exec python3 "$ROOT/scripts/bench.py" "$@"
