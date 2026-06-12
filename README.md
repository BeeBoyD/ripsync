# Ferry 🛶

**rsync's superpower, none of its footguns.**

Ferry makes a destination an exact mirror of a source by transferring **only what
changed** — like `rsync` — but it's written in safe Rust, ships a live TUI, has sane
flags, and is built to **beat rsync on trees of many tiny files** via a parallel walk and
parallel hashing.

> Status: Ferry v0.2 local sync. Networking is on the roadmap, not built yet.

## Why Ferry

1. **Speed.** Parallel filesystem walk (`jwalk`) + parallel hashing/copy (`rayon`). The
   target win is the 100k-tiny-files case where rsync's single-threaded pipeline stalls.
2. **Safety.** The 2025 rsync CVE class is *impossible by construction* here:
   - Heap overflow (CVE-2024-12084): core is safe Rust except for two reviewed
     `io_uring` submission blocks isolated in one Linux-only module.
   - Symlink path-traversal (CVE-2024-12087/12088): every write/symlink target is
     canonicalized and **must stay inside the destination root**, or Ferry refuses.
   - The classic "rsync wiped my disk": `--delete` aborts if the source is empty/unreadable
     and never deletes without `--yes`.
3. **UX.** Live `ratatui` dashboard, readable `--dry-run`, `--output json`, and a
   highlighted delete-preview panel before anything is removed.

## Install / build

```sh
cargo build --release
./target/release/ferry --help
```

## Usage

```sh
ferry <SRC> <DST> [FLAGS]
```

| Flag | Meaning |
|------|---------|
| `-n, --dry-run` | Plan only; print a readable summary; change nothing. |
| `--delete` | Mirror deletions (gated by `--yes`). |
| `--yes` | Confirm destructive actions. |
| `-c, --checksum` | Compare by content hash, not size+mtime. |
| `--delta` | Force delta transfer even locally (demo/bench). |
| `--reflink <auto\|always\|never>` | Copy-on-write clone on CoW filesystems (btrfs/XFS/APFS/ReFS). `auto` tries it and falls back; `always` requires it; `never` skips it. |
| `--fsync <auto\|always\|never>` | Durability vs speed. `auto`/`never` skip per-file fsync; `auto` still fsyncs touched directories once so renames survive a crash; `always` fsyncs every file before rename (slowest, strongest). |
| `--backend <auto\|uring\|portable>` | Copy backend. `auto` uses io_uring on Linux when available. |
| `--no-index` | Disable the default persistent destination index for incremental re-syncs. |
| `-H, --hard-links` | Preserve source hardlink groups. |
| `-S, --sparse` | Preserve sparse-file holes where the filesystem supports extent seeking. |
| `--xattrs` / `--acls` | Preserve extended attributes / POSIX ACL attributes. |
| `--owner` / `--group` | Preserve numeric uid/gid (requires suitable privileges). |
| `--exclude <PAT>` | Glob, repeatable. |
| `--bwlimit <RATE>` | Throttle (parsed now, enforced later). |
| `--partial` | Keep partial files for resume (later phase). |
| `--no-tui` | Plain line output instead of the TUI. |
| `--output <human\|json>` | Output format. |
| `-j, --threads <N>` | Parallelism (default: CPU count). |
| `-v` / `-q` | Verbosity. |

By default, local sync compares by **size + mtime** (like `rsync -a` locally). Pass
`--checksum` to compare by content, or `--delta` to exercise the delta engine.

## The delta engine

Ferry implements the rsync rolling-checksum algorithm by hand in `ferry-core::delta`:
a position-weighted weak checksum (O(1) to roll) narrows candidates, BLAKE3 confirms a
block match, and the encoder emits a compact stream of `Copy{block}` / `Literal{bytes}`
ops. `apply(old, encode(old, new)) == new` is property-tested for all inputs. It powers
`--delta`/`--checksum` today and is the foundation for over-the-wire transfer in Phase 5.

## Benchmarks

`scripts/bench.sh` generates the full datasets, times each backend, writes raw CSV, and
verifies every destination with `diff -rq` plus mode/mtime/symlink checks. These are
single measured runs from June 12, 2026; targets are not substituted for results.

Hardware: AMD Ryzen 7 9800X3D (8C/16T), 30 GiB RAM, Linux 7.0.3, rsync 3.4.2.
Tiny-file runs used `tmpfs`; the 10 GiB run used an NVMe-hosted NTFS volume through
FUSE (`fuseblk`). All rows are warm-cache, release builds, and default durability.

| Cache | Scenario | Tool | Wall | Files/s | GiB/s |
|-------|----------|------|-----:|--------:|------:|
| warm | 100k tiny, initial | Ferry uring | 0.692 s | 144,479 | 0.0023 |
| warm | 100k tiny, initial | **Ferry portable** | **0.351 s** | **285,173** | **0.0045** |
| warm | 100k tiny, initial | rsync `-a` | 0.675 s | 148,073 | 0.0023 |
| warm | 1M tiny, initial | Ferry uring | 6.372 s | 156,936 | 0.0025 |
| warm | 1M tiny, initial | **Ferry portable** | **3.440 s** | **290,658** | **0.0046** |
| warm | 1M tiny, initial | rsync `-a` | 6.282 s | 159,190 | 0.0025 |
| warm | 10 GiB / 500 files, initial | Ferry uring | 17.037 s | 29 | 0.587 |
| warm | 10 GiB / 500 files, initial | **Ferry portable** | **14.087 s** | **35** | **0.710** |
| warm | 10 GiB / 500 files, initial | rsync `-a` | 19.527 s | 26 | 0.512 |
| warm | 1M tree, 100 changed | Ferry uring + index | 3.330 s | 300,300 | 0.0048 |
| warm | 1M tree, 100 changed | Ferry portable + index | 3.386 s | 295,340 | 0.0047 |
| warm | 1M tree, 100 changed | **rsync `-a`** | **1.352 s** | **739,605** | **0.0117** |

Portable Ferry was 1.93× rsync at 100k files, 1.83× at 1M files, and 1.39× on
the 10 GiB copy. Forced uring did not improve tiny-file performance on this setup, and
the persistent index did not beat rsync on the measured incremental case. The re-sync
GiB/s column is logical tree bytes scanned per second, not bytes transferred.

Cold-cache numbers were not recorded: this host does not permit writing
`/proc/sys/vm/drop_caches` and has no passwordless `sudo`. CoW/reflink numbers were also
not recorded because neither available benchmark filesystem supports btrfs/XFS/APFS/ReFS
cloning. The harness refuses to label a run cold when cache dropping fails. Raw results
are in [`bench-results.csv`](bench-results.csv).

### Micro (criterion, `cargo bench -p ferry-core`)

| Bench | Time | Throughput |
|-------|-----:|-----------:|
| Rolling weak checksum, 1 MiB window-roll | 744 µs | ~1.4 GB/s |
| Delta encode, 1 MiB with a small change | 2.78 ms | ~360 MB/s |
| Delta apply, 1 MiB | 24 µs | ~43 GB/s |

Reproduce with `./scripts/bench.sh` (macro) and `cargo bench` (micro). Use
`CACHE_MODE=both` only on a host allowed to drop page cache.

## Unsafe-code audit

`ferry-core` has exactly two `unsafe` blocks. Both submit SQEs in
`crates/ferry-core/src/io/uring.rs`, both have adjacent `SAFETY` lifetime comments, and
no other source module permits unsafe code. Builds without the Linux `io-uring` feature retain
crate-level `#![forbid(unsafe_code)]`; feature-enabled builds use crate-level `deny` and
the isolated module's local `allow`.

## Roadmap

- **Ferry v0.2:** local sync, reflink/CFR/io_uring backends, persistent index,
  metadata preservation, TUI, parity tests, and scale benchmarks.
- **Phase 5:** remote sync over SSH — spawn `ferry` on the far end and run our own framed
  protocol with true over-the-wire delta transfer. *(No rsync wire-protocol compat; that
  is explicitly out of scope.)*
- **Later:** config profiles, watch mode, `--bwlimit` enforcement, and `--partial` resume.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
