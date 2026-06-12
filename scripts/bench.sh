#!/usr/bin/env bash
# Ferry v0.2 scale benchmark. Generates the requested datasets, records raw CSV
# metrics, and verifies content plus mode/mtime/symlink metadata after every run.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FERRY="$ROOT/target/release/ferry"
TINY_ROOT="${BENCH_TINY_ROOT:-/tmp/ferry-bench-v02}"
LARGE_ROOT="${BENCH_LARGE_ROOT:-$ROOT/.bench-large}"
RESULTS="${BENCH_RESULTS:-$ROOT/bench-results.csv}"
RUNS="${RUNS:-1}"
CACHE_MODE="${CACHE_MODE:-warm}" # warm, cold, or both
SCENARIOS="${BENCH_SCENARIOS:-tiny-100k,tiny-1m,large-10gib,resync}"

command -v rsync >/dev/null || { echo "rsync is required" >&2; exit 1; }
cargo build --release --manifest-path "$ROOT/Cargo.toml" >/dev/null
mkdir -p "$TINY_ROOT" "$LARGE_ROOT"

if [[ "${BENCH_APPEND:-0}" != 1 || ! -f "$RESULTS" ]]; then
  printf 'cache,scenario,tool,seconds,files,bytes,files_per_sec,gib_per_sec,filesystem\n' >"$RESULTS"
fi

drop_caches() {
  sync
  if [[ -w /proc/sys/vm/drop_caches ]]; then
    echo 3 >/proc/sys/vm/drop_caches
  elif sudo -n true >/dev/null 2>&1; then
    echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
  else
    echo "cold-cache run requested, but dropping page cache is not permitted" >&2
    return 1
  fi
}

generate_tiny() {
  local root="$1" count="$2"
  [[ -f "$root.complete-$count" ]] && return
  rm -rf "$root"
  echo ">> generating $count tiny files in $root"
  python3 - "$root" "$count" <<'PY'
import os, pathlib, sys
root = pathlib.Path(sys.argv[1])
count = int(sys.argv[2])
root.mkdir(parents=True)
for i in range(count):
    directory = root / f"{i // 1000:06d}"
    if i % 1000 == 0:
        directory.mkdir()
    (directory / f"{i % 1000:04d}.bin").write_bytes(f"{i:016x}\n".encode())
PY
  printf 'complete\n' >"$root.complete-$count"
}

generate_large() {
  local root="$1"
  [[ -f "$root.complete-10gib" ]] && return
  rm -rf "$root"
  mkdir -p "$root"
  echo ">> generating 10 GiB across 500 files in $root"
  dd if=/dev/urandom of="$root/.template" bs=1M count=20 status=none
  for i in $(seq -w 0 499); do
    cp --reflink=never "$root/.template" "$root/$i.bin"
  done
  rm "$root/.template"
  printf 'complete\n' >"$root.complete-10gib"
}

verify_tree() {
  local src="$1" dst="$2"
  diff -rq --exclude=.ferry "$src" "$dst" >/dev/null
  python3 - "$src" "$dst" <<'PY'
import os, pathlib, stat, sys
src, dst = map(pathlib.Path, sys.argv[1:])
for base, dirs, files in os.walk(src, followlinks=False):
    rel_base = pathlib.Path(base).relative_to(src)
    dirs[:] = [d for d in dirs if d != ".ferry"]
    for name in dirs + files:
        rel = rel_base / name
        a, b = os.lstat(src / rel), os.lstat(dst / rel)
        if stat.S_IFMT(a.st_mode) != stat.S_IFMT(b.st_mode):
            raise SystemExit(f"type mismatch: {rel}")
        if stat.S_IMODE(a.st_mode) != stat.S_IMODE(b.st_mode):
            raise SystemExit(f"mode mismatch: {rel}")
        if abs(a.st_mtime_ns - b.st_mtime_ns) > 1_000_000_000:
            raise SystemExit(f"mtime mismatch: {rel}")
        if stat.S_ISLNK(a.st_mode) and os.readlink(src / rel) != os.readlink(dst / rel):
            raise SystemExit(f"symlink mismatch: {rel}")
PY
}

elapsed() {
  python3 - "$1" "$2" <<'PY'
import sys
print((int(sys.argv[2]) - int(sys.argv[1])) / 1_000_000_000)
PY
}

record() {
  local cache="$1" scenario="$2" tool="$3" seconds="$4" files="$5" bytes="$6" fs="$7"
  python3 - "$cache" "$scenario" "$tool" "$seconds" "$files" "$bytes" "$fs" >>"$RESULTS" <<'PY'
import sys
cache, scenario, tool, seconds, files, size, fs = sys.argv[1:]
seconds, files, size = float(seconds), int(files), int(size)
fps = files / seconds
gibs = size / (1024 ** 3) / seconds
print(f"{cache},{scenario},{tool},{seconds:.6f},{files},{size},{fps:.2f},{gibs:.6f},{fs}")
PY
}

run_sync() {
  local tool="$1" backend="$2" src="$3" dst="$4"
  if [[ "$tool" == rsync ]]; then
    rsync -a "$src/" "$dst"
  else
    "$FERRY" "$src" "$dst" --no-tui -q --backend "$backend"
  fi
}

bench_initial() {
  local cache="$1" scenario="$2" src="$3" files="$4" bytes="$5" dest_root="$6"
  local fs
  fs="$(findmnt -n -T "$dest_root" -o FSTYPE)"
  for tool_backend in "ferry:uring" "ferry:portable" "rsync:none"; do
    IFS=: read -r tool backend <<<"$tool_backend"
    for run in $(seq 1 "$RUNS"); do
      local dst="$dest_root/dst-${scenario}-${tool}-${backend}-${run}"
      rm -rf "$dst"
      [[ "$cache" == cold ]] && drop_caches
      local start end seconds
      start="$(date +%s%N)"
      run_sync "$tool" "$backend" "$src" "$dst"
      end="$(date +%s%N)"
      seconds="$(elapsed "$start" "$end")"
      verify_tree "$src" "$dst"
      record "$cache" "$scenario" "${tool}-${backend}" "$seconds" "$files" "$bytes" "$fs"
      rm -rf "$dst"
    done
  done
}

mutate_100() {
  local src="$1" marker="$2"
  python3 - "$src" "$marker" <<'PY'
import pathlib, sys
root, marker = pathlib.Path(sys.argv[1]), sys.argv[2]
for path in sorted(root.glob("*/*.bin"))[:100]:
    path.write_text(f"changed-{marker}-{path.name}\n")
PY
}

bench_resync() {
  local cache="$1" src="$2" files="$3" bytes="$4"
  local fs
  fs="$(findmnt -n -T "$TINY_ROOT" -o FSTYPE)"
  local serial=0
  for tool_backend in "ferry:uring" "ferry:portable" "rsync:none"; do
    IFS=: read -r tool backend <<<"$tool_backend"
    for run in $(seq 1 "$RUNS"); do
      serial=$((serial + 1))
      local dst="$TINY_ROOT/dst-resync-${tool}-${backend}-${run}"
      rm -rf "$dst"
      run_sync "$tool" "$backend" "$src" "$dst"
      mutate_100 "$src" "$serial"
      [[ "$cache" == cold ]] && drop_caches
      local start end seconds
      start="$(date +%s%N)"
      run_sync "$tool" "$backend" "$src" "$dst"
      end="$(date +%s%N)"
      seconds="$(elapsed "$start" "$end")"
      verify_tree "$src" "$dst"
      record "$cache" resync-1m-100-changed "${tool}-${backend}" "$seconds" "$files" "$bytes" "$fs"
      rm -rf "$dst"
    done
  done
}

TINY_100K="$TINY_ROOT/src-tiny-100k"
TINY_1M="$TINY_ROOT/src-tiny-1m"
LARGE_10G="$LARGE_ROOT/src-large-10gib"
generate_tiny "$TINY_100K" 100000
generate_tiny "$TINY_1M" 1000000
generate_large "$LARGE_10G"

case "$CACHE_MODE" in
  warm) caches=(warm) ;;
  cold) caches=(cold) ;;
  both) caches=(warm cold) ;;
  *) echo "CACHE_MODE must be warm, cold, or both" >&2; exit 2 ;;
esac

for cache in "${caches[@]}"; do
  if [[ ",$SCENARIOS," == *",tiny-100k,"* ]]; then
    bench_initial "$cache" tiny-100k "$TINY_100K" 100000 1700000 "$TINY_ROOT"
  fi
  if [[ ",$SCENARIOS," == *",tiny-1m,"* ]]; then
    bench_initial "$cache" tiny-1m "$TINY_1M" 1000000 17000000 "$TINY_ROOT"
  fi
  if [[ ",$SCENARIOS," == *",large-10gib,"* ]]; then
    bench_initial "$cache" large-10gib "$LARGE_10G" 500 10737418240 "$LARGE_ROOT"
  fi
  if [[ ",$SCENARIOS," == *",resync,"* ]]; then
    bench_resync "$cache" "$TINY_1M" 1000000 17000000
  fi
done

echo "results: $RESULTS"
