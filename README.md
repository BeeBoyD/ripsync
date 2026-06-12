# Ferry 🛶

**rsync's superpower, none of its footguns.**

Ferry makes a destination an exact mirror of a source by transferring **only what
changed** — like `rsync` — but it's written in safe Rust, ships a live TUI, has sane
flags, and is built to **beat rsync on trees of many tiny files** via a parallel walk and
parallel hashing.

> Status: local sync milestone (Phases 0–4). Networking is on the roadmap, not built yet.

## Why Ferry

1. **Speed.** Parallel filesystem walk (`jwalk`) + parallel hashing/copy (`rayon`). The
   target win is the 100k-tiny-files case where rsync's single-threaded pipeline stalls.
2. **Safety.** The 2025 rsync CVE class is *impossible by construction* here:
   - Heap overflow (CVE-2024-12084): core crate is `#![forbid(unsafe_code)]`.
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

`scripts/bench.sh` drives `hyperfine` to compare Ferry vs `rsync -a`. Results table is
pasted here after Phase 4. <!-- BENCH_TABLE -->

## Roadmap

- **Phase 0–4 (this milestone):** workspace, delta engine, local sync engine, TUI,
  parity tests vs rsync, benches.
- **Phase 5:** remote sync over SSH — spawn `ferry` on the far end and run our own framed
  protocol with true over-the-wire delta transfer. *(No rsync wire-protocol compat; that
  is explicitly out of scope.)*
- **Phase 6:** config profiles, watch mode, `--bwlimit` enforcement, `--partial` resume,
  xattr/ACL/hardlink/sparse-file preservation.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
