# Performance

ripsync is fast because of design choices that hold across platforms: a
parallel walk, a copy ladder that prefers copy-on-write clones, a persistent
index that turns re-syncs into metadata diffs, and `foldhash` + memory-mapped
BLAKE3 for hashing. This page documents how we measure that — fairly — and the
numbers from the reference machines.

## Measurement rules

Performance claims are based on release builds, with the raw CSV committed
(`bench-results.csv`) and reproducible from `scripts/bench.py`. Every run
records hardware, OS/kernel, filesystem, cache state, ripsync revision, and
`rsync --version`. We use **at least ten repetitions** and report median, mean,
population standard deviation, minimum, and the 95th percentile. A run is never
labelled *cold* unless dropping the page cache actually succeeded.

## Fair-comparison method

The harness is built to avoid the usual benchmarking traps:

- **Same filesystem, same durability.** `rsync -a` and ripsync's default both
  skip per-file `fsync`, so neither is penalised for durability the other
  skips. The destination filesystem is recorded and identical for both tools.
- **CoW is isolated, not hidden.** ripsync is measured twice — `--reflink auto`
  (copy-on-write clones where the filesystem supports them) and
  `--reflink never` (the portable read/write path). rsync cannot reflink, so the
  `--reflink never` column is the honest engine-vs-engine comparison, and the
  `--reflink auto` column shows the additional advantage from the filesystem.
- **A modern opponent.** On macOS the system `rsync` is Apple's `openrsync`
  ("2.6.9 compatible"); the harness prefers a Homebrew `rsync` 3.x so the
  comparison reflects current rsync, not a decade-old shim.
- **Correctness gate.** After every timed run the harness verifies content plus
  mode, mtime, and symlink targets; a mismatch fails the run.

## Apple Silicon / APFS — v1.1

Ten warm-cache repetitions per configuration on:

- Apple silicon, 14 cores; 48 GiB RAM; macOS 26 (Darwin 25);
- APFS on the internal NVMe (a real, fsync-honouring filesystem — *not* a RAM
  disk, so absolute tiny-file times are dominated by per-file metadata cost);
- Homebrew `rsync` 3.4.4; release build, default allocator and durability.

Median wall time (population stddev in parentheses):

| Scenario | ripsync `--reflink auto` | ripsync `--reflink never` | rsync 3.4.4 |
|---|---:|---:|---:|
| 100k tiny files, initial | 14.44 s (0.20) | **11.21 s** (0.21) | 24.46 s (1.27) |
| 5 GiB / 250 files, initial | **0.05 s** (0.02) | 3.74 s (1.62) | 6.69 s (1.96) |
| 100k tree, 100 changed (re-sync) | 0.87 s (0.16) | **0.50 s** (0.19) | 0.53 s (0.03) |

Bold marks the fastest honest engine-vs-engine result (`rsync` cannot reflink).
Highlights: ripsync's portable engine is **~2.2× faster than rsync** on the
100k-file initial copy and **~1.8× faster** on large files; `clonefile`
(`--reflink auto`) clones 5 GiB in ~50 ms; and the persistent index makes the
changed-file re-sync as fast as rsync's own quick check while validating every
skipped entry. Full median/mean/stddev/min/p95 come from
`scripts/summarize_bench.py`; the raw rows are in `bench-results.csv`.

Reading the table: on identical APFS, ripsync's portable engine
(`--reflink never`) is faster than modern rsync on every scenario, the
persistent index makes the changed-file re-sync effectively instant, and large
files benefit further from `clonefile` under `--reflink auto`. For directories
of *many tiny* files on APFS, `clonefile`'s per-file overhead makes
`--reflink never` the quicker choice — a size-aware reflink heuristic (mirroring
the Linux io_uring selector) is the natural next optimisation.

### A correctness-and-speed fix this release

The portable buffered copy previously called `sync_all` on every file
unconditionally. On macOS that lowers to `F_FULLFSYNC` (a full drive-cache
flush), which made the non-reflink path roughly **30× slower** on many small
files — and it contradicted the documented `--fsync auto`/`never` contract
(skip per-file fsync; fsync touched directories once). Durability is now solely
the caller's responsibility, consistent across all three copy strategies. The
numbers above are post-fix.

## Linux / tmpfs and NVMe — v0.3 reference

Earlier five-repetition measurements on an AMD Ryzen 7 9800X3D (8c/16t,
30 GiB RAM, Linux 7.0.3, rsync 3.4.2), using `tmpfs` for the tiny-file scenarios
and NVMe `fuseblk` for the 10 GiB scenario. Because the tiny-file sets lived in
a RAM-backed `tmpfs`, these absolute numbers are not comparable to the APFS
table above, but they show the io_uring backend's reach on Linux.

| Scenario | ripsync uring | ripsync portable | rsync |
|---|---:|---:|---:|
| 100k tiny, initial | 0.568 s | 0.688 s | 0.657 s |
| 1M tiny, initial | 5.568 s | 6.889 s | 6.065 s |
| 10 GiB / 500, initial | 16.923 s | 18.281 s | 22.701 s |
| 1M tree, 100 changed | 1.328 s | 1.414 s | 1.346 s |

On Linux, `--backend auto` selects io_uring for a many-small-files workload
(≥ 4096 files with a median size below 64 KiB) and the portable ladder
otherwise; the choice and its reason are reported as `BackendSelected`.

## Reproduction

```sh
# Defaults: 10 warm reps; 100k + 500k tiny, 5 GiB large, 100k re-sync.
RUNS=10 ./scripts/bench.sh
./scripts/summarize_bench.py bench-results.csv
```

`scripts/bench.py` runs on macOS and Linux. Override dataset sizes, scenarios,
cache mode, and tool paths with the environment variables documented at the top
of that file. The harness verifies content and metadata after each run.

## Implementation notes

Incremental runs use a `foldhash` map lookup, a one-time parallel sort after the
walk, live stat validation for indexed skips, and journal updates that stat only
changed entries. `--checksum` and verification hash files at or above 16 MiB
with memory-mapped, rayon-parallel BLAKE3. The release profile is `lto = "fat"`,
`codegen-units = 1`, `strip = true`, `opt-level = 3`, with `panic = "unwind"`
retained so the RAII terminal guard and the io_uring / Windows-handle `Drop`
cleanups run on panic.
