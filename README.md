```
██████╗ ██╗██████╗ ███████╗██╗   ██╗███╗   ██╗ ██████╗
██╔══██╗██║██╔══██╗██╔════╝╚██╗ ██╔╝████╗  ██║██╔════╝
██████╔╝██║██████╔╝███████╗ ╚████╔╝ ██╔██╗ ██║██║     
██╔══██╗██║██╔═══╝ ╚════██║  ╚██╔╝  ██║╚██╗██║██║     
██║  ██║██║██║     ███████║   ██║   ██║ ╚████║╚██████╗
╚═╝  ╚═╝╚═╝╚═╝     ╚══════╝   ╚═╝   ╚═╝  ╚═══╝ ╚═════╝
```

**rsync's superpower, none of its footguns.**

[![CI](https://github.com/beeboyd/ripsync/actions/workflows/ci.yml/badge.svg)](https://github.com/beeboyd/ripsync/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ripsync.svg)](https://crates.io/crates/ripsync)
[![docs.rs](https://img.shields.io/docsrs/ripsync-core)](https://docs.rs/ripsync-core)
[![license](https://img.shields.io/crates/l/ripsync.svg)](#license)

![ripsync TUI demo](docs/assets/ripsync-demo.gif)

## 🚀 Overview

ripsync is a fast, memory-safe directory synchronization tool written in Rust,
with copy-on-write backends per platform (Linux io_uring/reflink, Windows ReFS,
macOS clonefile), a persistent index for quick re-syncs, an operator TUI,
optional post-copy verification, and **remote sync over SSH** with delta transfer
and zstd compression. Device-tier auto-tuning adapts thread count and buffers to
the machine, from a 2-core NAS to a many-core workstation. `--watch` keeps a
destination continuously mirrored, and rsync-style filters (`--include`,
`--filter`, `--files-from`) select exactly what to transfer. For cloud object
storage (S3 and compatible) see the sibling tool
[ripclone](https://github.com/beeboyd/ripclone).

The full documentation is an [mdBook](docs/SUMMARY.md) (deployed to GitHub
Pages); see the [installation matrix](docs/install.md) for every install method.

## 📦 Installation

### Quick Start (from source)

```sh
cargo build --release
./target/release/ripsync SOURCE DESTINATION
```

An optional short alias `rs` (same program) can be built with
`cargo build --features rs-alias`. It is off by default and packagers install it
only where it does not collide with the BSD `rs` reshape utility.

### Package Managers

See [installation matrix](docs/install.md) for:
- **Homebrew** (macOS/Linux): `brew install beeboyd/homebrew-tap/ripsync`
- **AUR** (Arch Linux): `paru -S ripsync-bin`
- **cargo** (all platforms): `cargo install ripsync`
- **winget** (Windows): `winget install ripsync`
- **Scoop** (Windows): `scoop install ripsync` (from extras bucket)

## 📚 Usage Examples

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

# Transfer only Rust files
ripsync SOURCE DESTINATION --include '*.rs' --exclude '*'

# Transfer exactly the paths in a list
ripsync SOURCE DESTINATION --files-from changed.txt

# Keep the destination continuously mirrored
ripsync SOURCE DESTINATION --watch
```

See [docs/filters.md](docs/filters.md) and [docs/watch.md](docs/watch.md).

The TUI starts automatically for interactive human output. Use `--no-tui`,
`--output json`, or `--quiet` for noninteractive operation.

## 🔗 Remote Sync (SSH)

Give a source or destination as `[user@]host:path` and ripsync transfers over
ssh, rsync-style — the local side runs `ssh host ripsync --server` and the two
ripsync peers speak a versioned binary protocol over the pipe. ripsync must be
installed on both ends; your existing ssh keys, agent, `~/.ssh/config`, and
`known_hosts` are all reused.

```sh
# Push a local tree to a remote host
ripsync ./site/ user@web01:/var/www/site

# Pull from a remote host
ripsync user@web01:/var/www/site ./backup

# Delta + zstd compression over a slow link, capped at 2 MiB/s
ripsync ./data backup@nas:/pool/data -z --bwlimit 2M

# Custom ssh command (e.g. a non-default port)
ripsync ./data host:/srv -e "ssh -p 2222"
```

Changed files transfer as rolling-checksum deltas; `--whole-file`/`-W` forces a
full copy. See [docs/remote.md](docs/remote.md) for the protocol and security
model.

## 🛡️ Safety Model

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

## 💻 TUI

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

## ⚙️ Important Options

| Option | Meaning |
|---|---|
| `--verify none|changed|all` | Post-copy verification; default `none` |
| `--delete` / `--yes` | Mirror destination-only entries / approve automation |
| `--exclude`, `--include`, `--filter` | Glob/ordered filters; `--filter` takes `"+ PAT"`/`"- PAT"` |
| `--files-from FILE` | Transfer exactly the source-relative paths listed in `FILE` |
| `--watch` / `--debounce MS` | Re-sync on change (local); coalesce events (default 300 ms) |
| `--checksum` | Classify files using BLAKE3 |
| `--backend auto|portable|uring` | `auto` is portable-first; uring is explicit |
| `--no-index` | Disable the persistent v3 destination index |
| `-H`, `-S` | Preserve hardlinks or sparse allocation |
| `--xattrs`, `--acls` | Preserve extended attributes or POSIX ACLs |
| `--owner`, `--group` | Preserve numeric uid or gid |
| `--output json` | Emit an additive machine-readable final report |
| `--profile auto\|low\|balanced\|high` | Device performance tier; `auto` detects from CPU/RAM |
| `-z`, `--compress` / `--compress-level N` | zstd-compress remote whole-file payloads |
| `--bwlimit RATE` | Throttle remote upload rate (bare = KiB/s; `K`/`M`/`G` suffixes) |
| `-e`, `--rsh CMD` | Remote shell for `host:path` transfers (default `ssh`) |
| `-W`, `--whole-file` | Transfer whole files instead of deltas (remote) |

`--bwlimit` applies to remote transfers (it is a no-op for local copies).
`--partial` is accepted but resume-from-partial is not yet implemented; writes are
always atomic regardless.

## 📊 Performance

**Benchmarks:** 10 warm-cache iterations on Apple silicon (14 cores, 48 GiB), macOS 26, APFS

Measured against Homebrew rsync 3.4.4. ripsync tested with (`--reflink auto`) and without (`--reflink never`).

| Scenario | ripsync (COW) | ripsync (no COW) | rsync 3.4.4 | Speedup |
|---|---:|---:|---:|---:|
| 100k tiny files, initial | 14.44 s | **11.21 s** | 24.46 s | **2.2×** |
| 5 GiB / 250 files, initial | **0.05 s** | 3.74 s | 6.69 s | **2.0×** |
| 100k tree, 100 changed (re-sync) | 0.87 s | **0.50 s** | 0.53 s | *~1.0×* |

**Key wins:**
- Copy-on-write clones (macOS `clonefile`, Linux `reflink`) reduce copy time from 6.69s → 0.05s
- Persistent index keeps re-syncs as fast as rsync's quick-check
- Portable engine is ~2.2× faster on tiny files, ~1.8× on large files

See [Full benchmark methodology](docs/performance.md) and [Raw data](bench-results.csv).

## 📖 Documentation

| Topic | Link |
|-------|------|
| Full Guide | [mdBook (GitHub Pages)](https://beeboyd.com/ripsync) |
| Architecture & Design | [docs/architecture.md](docs/architecture.md) |
| Safety Guarantees | [docs/safety.md](docs/safety.md) |
| TUI Controls | [docs/tui.md](docs/tui.md) |
| Benchmarks & Methodology | [docs/performance.md](docs/performance.md) |
| Contributing | [docs/contributing.md](docs/contributing.md) |
| Changelog | [CHANGELOG.md](CHANGELOG.md) |

## 🔒 Unsafe Code

The Linux-only `io_uring` module contains exactly **two reviewed** `unsafe` submission blocks. No new unsafe code is used elsewhere. All FFI is audited via `cargo-deny` and `cargo-auditable`.

## 📜 License

MIT OR Apache-2.0.
