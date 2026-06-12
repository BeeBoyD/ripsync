# Performance

## Measurement Rules

Performance claims must be based on release builds and raw CSV committed or
attached to the release. Record hardware, kernel/OS, filesystem, cache state,
ripsync revision, rsync version, allocator, backend, and durability settings.
Use at least five repetitions and report median plus population standard
deviation. Do not label a run cold unless page-cache dropping succeeded.

The v0.3 release gate is:

- at least 15% lower median wall time than the v0.2 ripsync baseline for a
  1,000,000-file tree with 100 changed files;
- no more than 5% regression in each existing initial-copy median.

## June 12, 2026 Results

The v0.3 suite ran five warm-cache repetitions per configuration on:

- AMD Ryzen 7 9800X3D, 8 cores / 16 threads;
- 30 GiB RAM;
- Linux 7.0.3;
- rsync 3.4.2;
- release build with the default allocator and durability;
- `tmpfs` for tiny-file scenarios;
- NVMe-hosted `fuseblk` for the 10 GiB scenario.

| Scenario | Tool | Median | Population stddev | Change from prior |
|---|---|---:|---:|---:|
| 100k tiny, initial | ripsync uring | 0.568 s | 0.010 s | -17.9% |
| 100k tiny, initial | ripsync portable | 0.688 s | 0.009 s | +96.2% |
| 100k tiny, initial | rsync | 0.657 s | 0.004 s | -2.7% |
| 1M tiny, initial | ripsync uring | 5.568 s | 0.129 s | -12.6% |
| 1M tiny, initial | ripsync portable | 6.889 s | 0.152 s | +100.2% |
| 1M tiny, initial | rsync | 6.065 s | 0.009 s | -3.4% |
| 10 GiB / 500, initial | ripsync uring | 16.923 s | 1.739 s | -0.7% |
| 10 GiB / 500, initial | ripsync portable | 18.281 s | 0.077 s | +29.8% |
| 10 GiB / 500, initial | rsync | 22.701 s | 1.063 s | +16.3% |
| 1M tree, 100 changed | ripsync uring | 1.328 s | 0.069 s | -60.1% |
| 1M tree, 100 changed | ripsync portable | 1.414 s | 0.115 s | -58.2% |
| 1M tree, 100 changed | rsync | 1.346 s | 0.038 s | -0.4% |

Negative change is faster. The indexed re-sync target passes for both ripsync
backends. The initial-copy guardrail passes for uring but fails for portable,
which is also the `auto` selection in v0.3. Therefore the complete release
performance gate is not met.

Every measured destination passed the harness's content, mode, mtime, and
symlink verification. Cold-cache measurements were not run.

## Reproduction

```sh
RUNS=5 CACHE_MODE=warm ./scripts/bench.sh
./scripts/summarize_bench.py bench-results.csv
```

The harness verifies content, mode, mtime, and symlink targets after each run.
The 1M/100-change scenario should be run on the same filesystem and cache mode
as its baseline. Logical tree GiB/s on a re-sync is not transfer throughput and
must be labeled accordingly.

## Backend auto-selection (v0.4)

`--backend auto` resolves per platform and, on Linux, per workload:

- **Linux:** after planning, ripsync inspects the file set. If it has at least
  **4096** files **and** the median file size is below **64 KiB**, it selects the
  `io_uring` backend to amortize syscall overhead across many tiny copies;
  otherwise it uses the portable ladder. The median is computed in O(n) with
  `select_nth_unstable`, so the check is negligible next to the copy itself.
- **macOS / other Unix:** the portable `clonefile` / `copy_file_range` / buffered
  ladder.
- **Windows:** the ReFS block-clone / `CopyFileExW` backend.

The decision (and its reason) is reported as the `BackendSelected` event and in
`--stats` / JSON output. It is always overridable with an explicit `--backend`.
Thresholds live in `apply.rs` (`AUTO_URING_MIN_FILES`, `AUTO_URING_MEDIAN_MAX`).

## v0.4 Performance Changes

- Manifest and plan-classification maps use `foldhash` instead of SipHash. Keys
  are local paths and `(dev, ino)` pairs, never attacker-controlled, so the
  faster non-DoS-hardened hash is safe.
- `--checksum` and verification hash files at or above 16 MiB with
  `blake3::Hasher::update_mmap_rayon` (memory-mapped, rayon-parallel); smaller
  files stream through a single buffer.
- The Linux portable large-file path issues `posix_fadvise(SEQUENTIAL)` +
  `WILLNEED` (via `rustix`) for files ≥ 8 MiB and copies with a 1 MiB buffer.
- Release profile: `lto = "fat"`, `codegen-units = 1`, `strip = true`,
  `opt-level = 3`. `panic = "unwind"` is retained so the RAII terminal guard and
  the io_uring / Windows-handle `Drop` cleanups run on panic.

## Implementation Notes

Incremental runs use a `foldhash` map lookup, a one-time parallel sort after each
walk, live stat validation for indexed skips, and journal updates that stat only
changed entries. Initial sync still writes one complete atomic snapshot.
