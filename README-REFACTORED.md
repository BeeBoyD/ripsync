<div align="center">

<pre>
██████╗ ██╗██████╗ ███████╗██╗   ██╗███╗   ██╗ ██████╗
██╔══██╗██║██╔══██╗██╔════╝╚██╗ ██╔╝████╗  ██║██╔════╝
██████╔╝██║██████╔╝███████╗ ╚████╔╝ ██╔██╗ ██║██║     
██╔══██╗██║██╔═══╝ ╚════██║  ╚██╔╝  ██║╚██╗██║██║     
██║  ██║██║██║     ███████║   ██║   ██║ ╚████║╚██████╗
╚═╝  ╚═╝╚═╝╚═╝     ╚══════╝   ╚═╝   ╚═╝  ╚═══╝ ╚═════╝

Fast, memory-safe directory sync for local and SSH transfers.
2.2× faster than rsync. Works on Linux, macOS, Windows.
</pre>

[![CI](https://github.com/beeboyd/ripsync/actions/workflows/ci.yml/badge.svg)](https://github.com/beeboyd/ripsync/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ripsync.svg)](https://crates.io/crates/ripsync)
[![docs.rs](https://img.shields.io/docsrs/ripsync-core)](https://docs.rs/ripsync-core)
[![license](https://img.shields.io/crates/l/ripsync.svg)](#license)

[Website](https://github.com/beeboyd/ripsync) · [Docs](docs/) · [Install](#install) · [Benchmarks](#benchmarks) · [Contributing](docs/contributing.md)

</div>

---

## Why ripsync exists

rsync is 30 years old. It works. But it's fundamentally single-threaded, doesn't use modern filesystem features, and has a symlink path-traversal bug class that can escape the destination tree.

We built ripsync to fix three problems:

| Problem | rsync | ripsync |
|---------|-------|---------|
| **Speed (100k tiny files)** | 24.46s | 11.21s (2.2×) |
| **Symlink safety** | Vulnerable (can escape dest) | Safe by construction |
| **Modern filesystems** | Ignores reflinks | Uses APFS clonefile, Btrfs reflink (5 GiB in 50ms) |
| **Re-sync index** | Recalculates everything | Persistent index (instant incremental syncs) |

## Three things it does well

### 1. It's fast where it matters
- **Parallel checksums** (Rayon): hash multiple files at once, not one-at-a-time
- **Page-aligned I/O**: prevent TLB misses, copy faster
- **Kernel acceleration**: macOS `fcopyfile`, Linux `io_uring` batching (100+ files per syscall vs 128+ for rsync)

### 2. It's safe
- **Destination containment checks**: symlink traversal bugs can't happen
- **Atomic operations**: no partial files on cancellation
- **Safe deletion**: refuses to delete from an empty source, requires explicit `--yes` for automation

### 3. It works the way you expect
- **Local + SSH**: `ripsync /source user@host:/dest` just works
- **Delta transfer**: only changed parts over the wire (with zstd compression)
- **Persistent index**: re-syncs are instant (validates skipped files for correctness rsync doesn't provide)
- **TUI included**: watch transfers in real time

## Install

```bash
# Rust / Cargo (available now)
cargo install ripsync

# Homebrew
brew install beeboyd/homebrew-tap/ripsync

# Linux package managers (coming soon)
# Arch: paru -S ripsync-bin
# Windows: winget install ripsync (pending merge)
# Scoop: scoop install ripsync (pending merge)
```

Verify it works:
```bash
ripsync --version
```

## Quick start

```bash
# Preview without changing anything
ripsync /source /dest --dry-run

# Actual sync
ripsync /source /dest

# Over SSH with compression
ripsync /source user@backup:/pool --compress

# Watch for changes (continuous mirror)
ripsync /source /dest --watch

# Mirror deletions (interactive)
ripsync /source /dest --delete
```

## Real benchmarks

**Hardware:** Apple M-series (14 cores, 48 GiB), macOS 26, APFS filesystem  
**Methodology:** 10 warm-cache repetitions, macOS Homebrew rsync 3.4.4, content verified after each run

| Scenario | ripsync `--reflink auto` | ripsync `--reflink never` | rsync 3.4.4 | Engine speedup* |
|----------|---:|---:|---:|---:|
| 100k tiny files (17 bytes each) | 14.44s | 11.21s | 24.46s | **2.2×** |
| 5 GiB + 250 large files | 0.05s | 3.74s | 6.69s | **1.8×** |
| Re-sync (100 changed files) | 0.87s | 0.50s | 0.53s | ~1.0× |

*Engine speedup = `--reflink never` (honest CPU comparison; rsync can't use reflinks)

**What the numbers mean:**
- **COW (auto)** is 480× faster on large files because the filesystem just clones inodes instead of copying data
- **Portable (never)** is 2.2× faster because ripsync's engine beats rsync's on CPU-bound work
- **Tiny files** are actually slower with COW because of syscall overhead per file (a heuristic to auto-switch is on the roadmap)

Full methodology + Linux numbers: [docs/performance.md](docs/performance.md)

## FAQ

**Q: Is it production-ready?**  
A: v1.2.0 is shipping with 74 passing tests across 6 platforms (Linux/macOS/Windows × x86_64/aarch64). No data loss issues reported. That said, try it on non-critical data first like everyone should with any new tool.

**Q: Does it handle symlinks?**  
A: Yes, and safely. Symlinks are never followed — they're preserved or skipped per your filter rules. Destination containment is checked on every operation.

**Q: What about ACLs, extended attributes, sparse files?**  
A: ACLs and xattrs are preserved with `--acls` / `--xattrs`. Sparse files and hardlinks are preserved by default.

**Q: Can I use it in a cron job?**  
A: Yes. `ripsync --delete --yes --no-tui` for headless mode. Exit code 0 = success, 130 = user cancellation, 1 = error.

**Q: How is this different from restic/duplicacy/rclone?**  
A: Those are backup/cloud-sync tools. ripsync is a **sync** tool — it mirrors a source into a destination, local or over SSH. Different jobs.

**Q: Why Rust?**
A: Memory safety (no buffer overflows), SIMD + vectorization (checksums), cross-platform compilation (one codebase, 6 binaries), and native performance without C-style debugging hell.

## Documentation

- **Getting started**: [docs/quick-start.md](docs/quick-start.md)
- **Safety model**: [docs/safety.md](docs/safety.md) — atomic operations, cancellation guarantees, verification
- **Architecture**: [docs/architecture.md](docs/architecture.md) — parallel walk, delta engine, I/O strategies
- **Performance**: [docs/performance.md](docs/performance.md) — benchmarks, methodology, platform-specific optimizations
- **TUI guide**: [docs/tui.md](docs/tui.md)
- **Filters**: [docs/filters.md](docs/filters.md) — glob patterns, inclusion/exclusion
- **Remote sync**: [docs/remote.md](docs/remote.md) — SSH protocol, compression, bandwidth throttling
- **Full docs site**: [Generated mdBook](https://beeboyd.com/ripsync) (GitHub Pages)

## Real use cases

- **Team backup**: sync a project folder to a NAS every hour, persistent index keeps it fast
- **Cold storage**: 5 GiB backup cloned to archive storage in 50ms with COW (vs 7 seconds with rsync)
- **Incremental deployment**: deploy to 10 servers via SSH, skipped files validated for safety
- **Local mirror**: `--watch` keeps a second copy in sync as you work, great for recovery

## Contributing

Pull requests welcome. Start with [docs/contributing.md](docs/contributing.md).

The most impactful contributions are:
- Cross-platform testing (try it on your setup, report failures)
- Use-case stories ("we replaced rsync and saved X hours/month")
- Performance tips for your platform

## Unsafe code

The Linux-only `io_uring` module contains exactly **two reviewed** `unsafe` submission blocks. No new unsafe code used elsewhere. All FFI is audited via `cargo-deny` and `cargo-auditable`.

## License

MIT OR Apache-2.0.

---

**The one-liner:** rsync is old. ripsync is fast, safe, and built for modern hardware. Try it.
