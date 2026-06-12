#!/usr/bin/env bash
# Macro-benchmarks: ferry vs rsync. Uses hyperfine when available, otherwise a
# simple best-of-N timer. Run from the repo root: ./scripts/bench.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FERRY="$ROOT/target/release/ferry"
WORK="${BENCH_DIR:-/tmp/ferry-bench}"
RUNS="${RUNS:-5}"

echo ">> building ferry (release)"
cargo build --release --manifest-path "$ROOT/Cargo.toml" >/dev/null

command -v rsync >/dev/null || { echo "rsync not installed"; exit 1; }
HAVE_HF=0; command -v hyperfine >/dev/null && HAVE_HF=1

# bench NAME SRC -- runs ferry and rsync mirroring SRC into fresh dests.
bench() {
  local name="$1" src="$2"
  local df="$WORK/dst_ferry_$name" dr="$WORK/dst_rsync_$name"
  echo
  echo "=== $name ==="
  if [[ "$HAVE_HF" == 1 ]]; then
    hyperfine --warmup 1 --runs "$RUNS" --prepare "rm -rf $df $dr" \
      --command-name ferry "$FERRY $src $df --no-tui -q" \
      --command-name rsync "rsync -a $src/ $dr"
  else
    timeit ferry "$FERRY" "$src" "$df"
    timeit rsync rsync   "$src/" "$dr"
  fi
}

# timeit LABEL BIN SRC DST  (manual best-of-N wall clock)
timeit() {
  local label="$1" bin="$2" src="$3" dst="$4" best=99999 t
  for _ in $(seq "$RUNS"); do
    rm -rf "$dst"
    local start end
    start=$(date +%s.%N)
    if [[ "$bin" == rsync ]]; then rsync -a "$src" "$dst"; else "$bin" "$src" "$dst" --no-tui -q; fi
    end=$(date +%s.%N)
    t=$(echo "$end - $start" | bc)
    (( $(echo "$t < $best" | bc -l) )) && best=$t
  done
  printf "  %-6s %8.3f s (best of %s)\n" "$label" "$best" "$RUNS"
}

mkdir -p "$WORK"

# (c) 100k tiny files — the target win.
TINY="$WORK/src_tiny"
if [[ ! -d "$TINY" ]]; then
  echo ">> generating 100k tiny files"
  mkdir -p "$TINY"
  for d in $(seq 0 99); do
    mkdir -p "$TINY/$d"
    for f in $(seq 0 999); do echo "x" > "$TINY/$d/$f"; done
  done
fi
bench tiny100k "$TINY"

# (a) large tree: fewer, bigger files.
BIG="$WORK/src_big"
if [[ ! -d "$BIG" ]]; then
  echo ">> generating large tree (500 x 1MiB)"
  mkdir -p "$BIG"
  for f in $(seq 0 499); do head -c 1048576 /dev/urandom > "$BIG/$f.bin"; done
fi
bench big500 "$BIG"

# (b) re-sync after a tiny change (ferry/rsync should both be fast; skip-heavy).
echo
echo "=== resync_tiny_change (incremental) ==="
date > "$TINY/0/0"
if [[ "$HAVE_HF" == 1 ]]; then
  hyperfine --warmup 1 --runs "$RUNS" \
    --command-name ferry "$FERRY $TINY $WORK/dst_ferry_tiny100k --no-tui -q" \
    --command-name rsync "rsync -a $TINY/ $WORK/dst_rsync_tiny100k"
else
  timeit ferry "$FERRY" "$TINY" "$WORK/dst_ferry_tiny100k"
  timeit rsync rsync   "$TINY/" "$WORK/dst_rsync_tiny100k"
fi

echo
echo "done. (set RUNS=N, BENCH_DIR=path to tune)"
