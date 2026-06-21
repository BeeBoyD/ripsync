# AGENTS.md — ripsync Dev Guide

> Compact instruction file for future OpenCode sessions. Only includes facts an agent would likely miss.

## Workspace Structure

**Monorepo with 2 crates (Cargo workspace v2 resolver):**

- **`crates/ripsync-core/`** — Library. Core sync engine, all platform abstractions, safe by construction.
  - Entry: `src/lib.rs`
  - Exposes `AsyncSyncer`, `ChecksumEngine`, `IndexedWalk`, `DeltaTransfer`
  - **All unsafe code confined to `src/io/{uring,windows}.rs`** (CI enforces this with grep on every PR)
  - Tests: `tests/` (integration) + `benches/` (criterion)
  
- **`crates/ripsync/`** — Binary CLI. TUI, argument parsing, SSH transport.
  - Entry: `src/main.rs` (clap CLI)
  - Depends on ripsync-core
  - No unsafe code

- **`xtask/`** — Dev task runner. Default: generate man page + shell completions.
  - Run: `cargo xtask` or `cargo xtask dist-assets`
  - Outputs to `dist/assets/` (committed; used in releases)

## Commands

**Format + lint:**
```bash
cargo fmt --all              # Enforce 100-char line width (rustfmt.toml)
cargo clippy --all-targets --all-features -- -D warnings
```

**Test (runs on CI across 3 OSes):**
```bash
cargo test --all --all-features     # Full suite (74 tests)
cargo build -p ripsync --no-default-features --features system-malloc  # Minimal config
```

**Specific test:**
```bash
cargo test -p ripsync-core walk::tests::symlink_containment -- --nocapture
```

**Benchmarks (criterion):**
```bash
cargo bench -p ripsync-core --bench checksum
```

**Build release binary:**
```bash
cargo build -p ripsync --release   # Optimized: LTO, single codegen unit, fat, stripped
```

**Generate assets (man + shell completions):**
```bash
cargo xtask dist-assets
```

**Supply chain audit:**
```bash
cargo deny check advisories licenses bans sources
```

## Key Architecture Decisions

### Platform I/O Abstraction (`src/io/`)

Four I/O strategies, selected at runtime per platform:

| File | Platform | Method | When |
|------|----------|--------|------|
| `mod.rs` | All | Trait dispatch | Selects OS-specific impl |
| `uring.rs` | Linux ≥5.19 | io_uring + splice | Copy-on-write reflinks when avail; batches 100+ syscalls |
| `macos.rs` | macOS | fcopyfile(F_NOCACHE) | Instant 50ms on APFS via clonefile reflinks |
| `windows.rs` | Windows | SetFilePointerEx + ReadFile | Falls back to normal copy; ReFS detects reflinks internally |

**Unsafe pattern:** Two reviewed `unsafe { libc::ioctl(...) }` blocks only (io_uring & Windows handles). CI audits for violations.

### Index & Re-sync

- Persistent `.ripsync-index.bin` (bincode format) in destination root
- Stores: file paths, mtimes, sizes, blake3 hashes (256-bit rolling)
- Re-syncs validate index without full walk (instant on unchanged files)
- Deleted sources are safe: refuses to delete if source tree is empty

### Checksum Engine

- **Vectorized 8-byte rolling hash** (blake3 with SIMD, 8 GiB/s on M-series)
- Rayon parallelizes file hashing across CPUs
- Mmap used for large files to avoid copy

### Symlink Safety

- **All links preserved, never followed** (per rsync `--links` behavior)
- **Destination containment check:** every symlink target validated to not escape dest root
- No path traversal bugs possible by construction

### Memory & Allocator

- Default: **mimalloc** (faster, lower fragmentation)
- Fallback: `--features system-malloc` for ARM Windows cross-compile and minimal builds
- Profile release: fat LTO, codegen-units=1, unwinding enabled (RAII cleanup on panic; don't change to abort)

## Testing Quirks

### Cross-Platform Test Matrix

CI runs tests on `[ubuntu-latest, macos-latest, windows-latest]` × `[full-features, --no-default-features --features system-malloc]`:

- **Linux:** Full suite including `parity-vs-rsync` benchmarks (rsync must be installed; no-ops if absent)
- **macOS/Windows:** All tests except rsync parity suite

### Snapshot Tests

None. Use criterion benchmarks for perf regression detection.

### Fixtures

Integration tests use `tempfile` crate (auto-cleanup). No shared fixtures; each test is independent.

### Flaky Tests

None known. If a test fails intermittently, file issue with reproduction steps.

## Release Process

**Tag-driven (`v*` on main triggers release workflow):**

1. Push tag: `git tag v1.2.0 && git push --tags`
2. CI builds 6 cross-platform binaries + man page + completions
3. Generates SHA256 checksums, publishes to GitHub Releases
4. Homebrew tap auto-publishes (in `beeboyd/homebrew-tap`)
5. crates.io publishes via `cargo publish` (Cargo.toml `version.workspace`)

See `docs/releasing.md` for full process.

## Dependencies & Supply Chain

**Locked in `Cargo.lock`** (committed for reproducibility).

**Key direct deps:**
- `blake3` (hashing, mmap, rayon) — SIMD-accelerated
- `rayon` (parallelism)
- `clap` (CLI)
- `ratatui` (TUI, cross-platform)
- `io-uring` (Linux kernel acceleration)
- `windows-sys` (Windows FFI)
- `rustix` (safe syscall wrapper)

**Build deps:** criterion, proptest, assert_cmd, tempfile

**Denied:**
- No multiple versions of same crate (deny.toml)
- Wildcard versions disallowed (must be exact or `^`)
- Licenses: MIT, Apache-2.0, BSD-*, ISC, Zlib, MPL-2.0, Unicode-* only
- Advisory check: yanked versions forbidden

Run `cargo deny check` before every release.

## Formatting & Style

- **Line width:** 100 chars (rustfmt.toml, enforced in CI)
- **Edition:** 2024 (rust-version MSRV: 1.85)
- **Warnings as errors:** RUSTFLAGS="-D warnings" on CI
- **Unsafe audit:** Only in `src/io/{uring,windows}.rs`

## Known Constraints

1. **Symlink re-sync is slower** (must re-validate destination containment on every pass)
2. **Tiny files (< 4 KiB) are faster with `--reflink never`** (COW syscall overhead > copy overhead for small data)
3. **index invalidation:** If dest is manually edited, delete `.ripsync-index.bin` to force full re-walk
4. **Windows ARM64 cross-compile:** Must use `--no-default-features --features system-malloc` to avoid building mimalloc C code for arm64

## Code Comments

- Unsafe blocks: Always justified with SAFETY comment
- Platform-specific: Prefixed with `// MACOS:`, `// LINUX:`, `// WINDOWS:`
- Mutex poison handling: Uses `.unwrap_or_else(|e| e.into_inner())` everywhere (17 sites, verified)
- IO operations: Logged with tracing at TRACE level (enable with `RUST_LOG=trace`)

## Git Workflow

- **Branch protection:** master requires 1 PR approval + status checks passing
- **Merge strategy:** Squash + rebase only (no merge commits)
- **Head branches:** Auto-deleted on merge

## Common Debugging Commands

```bash
# Full build with all platform features
cargo build --all-features -vv

# Run single test with backtrace
RUST_LOG=trace RUST_BACKTRACE=1 cargo test -p ripsync-core walk::tests::symlink_containment -- --nocapture --test-threads=1

# Check for unsafe violations
grep -rn "unsafe {" crates/ripsync-core/src | grep -vE "io/(uring|windows)\.rs"

# Benchmark a specific function
cargo bench -p ripsync-core --bench checksum -- --exact blake3

# Profile a release build (macOS)
cargo instruments -p ripsync --release -- sync /tmp/test-source /tmp/test-dest
```

## Files to Read First When Confused

1. `README.md` — Problem statement, benchmarks, honest trade-offs
2. `docs/architecture.md` — Data flow, platform decisions
3. `docs/safety.md` — Mutex patterns, atomic guarantees
4. `crates/ripsync-core/src/lib.rs` — Public API surface
5. `crates/ripsync-core/src/io/mod.rs` — Platform dispatch logic
6. `.github/workflows/ci.yml` — Exactly what runs on PR
