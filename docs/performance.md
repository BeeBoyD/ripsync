# Performance

## Measurement Rules

Performance claims must be based on release builds and raw CSV committed or
attached to the release. Record hardware, kernel/OS, filesystem, cache state,
Ferry revision, rsync version, allocator, backend, and durability settings.
Use at least five repetitions and report median plus population standard
deviation. Do not label a run cold unless page-cache dropping succeeded.

The v0.3 release gate is:

- at least 15% lower median wall time than the v0.2 Ferry baseline for a
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
| 100k tiny, initial | Ferry uring | 0.568 s | 0.010 s | -17.9% |
| 100k tiny, initial | Ferry portable | 0.688 s | 0.009 s | +96.2% |
| 100k tiny, initial | rsync | 0.657 s | 0.004 s | -2.7% |
| 1M tiny, initial | Ferry uring | 5.568 s | 0.129 s | -12.6% |
| 1M tiny, initial | Ferry portable | 6.889 s | 0.152 s | +100.2% |
| 1M tiny, initial | rsync | 6.065 s | 0.009 s | -3.4% |
| 10 GiB / 500, initial | Ferry uring | 16.923 s | 1.739 s | -0.7% |
| 10 GiB / 500, initial | Ferry portable | 18.281 s | 0.077 s | +29.8% |
| 10 GiB / 500, initial | rsync | 22.701 s | 1.063 s | +16.3% |
| 1M tree, 100 changed | Ferry uring | 1.328 s | 0.069 s | -60.1% |
| 1M tree, 100 changed | Ferry portable | 1.414 s | 0.115 s | -58.2% |
| 1M tree, 100 changed | rsync | 1.346 s | 0.038 s | -0.4% |

Negative change is faster. The indexed re-sync target passes for both Ferry
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

## Implementation Notes

Incremental runs use `HashMap` lookup, a one-time parallel sort after each walk,
live stat validation for indexed skips, and journal updates that stat only
changed entries. Initial sync still writes one complete atomic snapshot.
