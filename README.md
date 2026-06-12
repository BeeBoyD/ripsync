# ripsync

[![CI](https://github.com/beeboyd/ripsync/actions/workflows/ci.yml/badge.svg)](https://github.com/beeboyd/ripsync/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ripsync.svg)](https://crates.io/crates/ripsync)
[![docs.rs](https://img.shields.io/docsrs/ripsync-core)](https://docs.rs/ripsync-core)
[![license](https://img.shields.io/crates/l/ripsync.svg)](#license)

> rsync's superpower, none of its footguns.

![ripsync TUI demo](docs/assets/ripsync-demo.gif)

ripsync is a fast, memory-safe local directory synchronization tool written in
Rust, with copy-on-write backends per platform (Linux io_uring/reflink, Windows
ReFS, macOS clonefile), a persistent index for quick re-syncs, an operator TUI,
and optional post-copy verification. Remote sync and watch mode are not
implemented.

The full documentation is an [mdBook](docs/SUMMARY.md) (deployed to GitHub
Pages); see the [installation matrix](docs/install.md) for every install method.

## Quick Start

```sh
cargo build --release
./target/release/ripsync SOURCE DESTINATION
```

An optional short alias `rs` (same program) can be built with
`cargo build --features rs-alias`. It is off by default and packagers install it
only where it does not collide with the BSD `rs` reshape utility.

Useful examples:

```sh
# Preview without changing the destination
ripsync SOURCE DESTINATION --dry-run

# Mirror deletions in an interactive terminal
ripsync SOURCE DESTINATION --delete

# Automation must approve deletion explicitly
ripsync SOURCE DESTINATION --delete --yes --no-tui

# Hash changed files after copying
ripsync SOURCE DESTINATION --verify changed

# Compare complete trees after copying
ripsync SOURCE DESTINATION --verify all
```

The TUI starts automatically for interactive human output. Use `--no-tui`,
`--output json`, or `--quiet` for noninteractive operation.

## Safety Model

- File updates use a temporary file and atomic rename.
- Every destination operation is containment-checked.
- In-flight atomic operations finish on cancellation; no later work starts.
- Cancellation keeps completed changes, removes temporary files, skips later
  phases, does not update the manifest, and exits with status `130`.
- `--delete` refuses an empty source. Interactive deletion requires typing
  `DELETE`; automation requires `--yes`.
- Verification runs before manifest persistence. A mismatch returns nonzero and
  leaves the previous manifest intact.

See [docs/safety.md](docs/safety.md) for details.

## TUI

The lifecycle views cover planning, review, copying, deleting, verifying,
finalizing, completion, cancellation, and failure. Controls include:

| Key | Action |
|---|---|
| `Tab`, `1`-`4` | Switch views |
| `p` | Pause or resume engine work |
| `q`, `Ctrl-C` | Open graceful-cancel confirmation |
| `j/k`, arrows, Page Up/Down, Home/End | Navigate |
| `/` | Filter the current list |
| `f` | Cycle event type filters |
| `?` | Show key reference |
| `Esc` | Close an overlay or clear a filter |

`NO_COLOR` disables semantic colors. Small terminals use a compact layout.
See [docs/tui.md](docs/tui.md).

## Important Options

| Option | Meaning |
|---|---|
| `--verify none|changed|all` | Post-copy verification; default `none` |
| `--delete` / `--yes` | Mirror destination-only entries / approve automation |
| `--checksum` | Classify files using BLAKE3 |
| `--backend auto|portable|uring` | `auto` is portable-first; uring is explicit |
| `--no-index` | Disable the persistent v3 destination index |
| `-H`, `-S` | Preserve hardlinks or sparse allocation |
| `--xattrs`, `--acls` | Preserve extended attributes or POSIX ACLs |
| `--owner`, `--group` | Preserve numeric uid or gid |
| `--output json` | Emit an additive machine-readable final report |

`--bwlimit` and `--partial` are recognized placeholders but fail immediately
because throttling and partial resume are not implemented.

## Performance

Warm-cache measurements on an AMD Ryzen 7 9800X3D with 30 GiB RAM, Linux 7.0.3,
and rsync 3.4.2. Tiny-file runs used `tmpfs`; the 10 GiB run used `fuseblk`.

| Scenario | ripsync uring median | ripsync portable median | rsync median |
|---|---:|---:|---:|
| 100k tiny, initial | 0.591 s | 0.717 s | 0.658 s |
| 1M tiny, initial | 5.568 s | 6.889 s | 6.065 s |
| 10 GiB / 500 files, initial | 16.923 s | 18.281 s | 22.701 s |
| 1M tree, 100 changed | 1.328 s | 1.414 s | 1.346 s |

The 100k row was re-measured for v0.4 (3 warm runs) after the `foldhash`,
`blake3` mmap, `fadvise`, and `lto = "fat"` changes and is within run-to-run
variance of the v0.3 baseline (0.568 / 0.688 s) — no regression. The 1M and
10 GiB rows carry over from the v0.3 measurement; those constant factors are
unchanged by the v0.4 work, and the full-scale suite is the release-gate
measurement (run it on a host with adequate scratch space).

Raw rows are in [bench-results.csv](bench-results.csv). Run
`scripts/summarize_bench.py bench-results.csv` for medians and population
standard deviation. See [docs/performance.md](docs/performance.md).

## Documentation

- [Architecture](docs/architecture.md)
- [Safety](docs/safety.md)
- [TUI](docs/tui.md)
- [Performance](docs/performance.md)
- [Contributing](docs/contributing.md)
- [Changelog](CHANGELOG.md)

## Unsafe Code

The Linux-only io_uring module contains exactly two reviewed `unsafe` submission
blocks. No new unsafe code is used elsewhere.

## License

MIT OR Apache-2.0.
