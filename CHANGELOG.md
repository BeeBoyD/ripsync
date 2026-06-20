# Changelog

## 1.1.0 - 2026-06-20

### Added

- **Filters.** New `--include`, `--filter`, and `--files-from` on top of the
  existing `--exclude`. Rules are matched against the relative path, first match
  wins, default-include; precedence is `--filter` (ordered `+`/`-`) → `--include`
  → `--exclude`. `--exclude` now also drops everything beneath a matching
  directory. The unified matcher lives in `ripsync_core::filter::Filter`.
- **Watch mode.** `--watch` re-runs the incremental sync whenever the source
  changes, coalescing bursts over a `--debounce` window (default 300 ms). Local
  transfers only; Ctrl-C stops the loop.

### Fixed

- **Portable copy no longer force-fsyncs every file.** The buffered copy used to
  call `sync_all` unconditionally, which both double-fsynced in
  `--fsync always` and violated the `auto`/`never` "skip per-file fsync"
  contract. On macOS that `sync_all` lowered to `F_FULLFSYNC` (a full drive
  flush), making the non-reflink path pathologically slow on many small files
  (≈30× on the benchmark). Durability is now solely the caller's responsibility,
  consistent across the reflink / `copy_file_range` / buffered strategies.

## 1.0.0 - 2026-06-20

### Added

- **Remote sync over SSH.** `ripsync [user@]host:path …` transfers to or from a
  remote host, rsync-style: the local side spawns `ssh host ripsync --server` and
  the two ripsync peers speak a versioned binary protocol over the ssh pipe
  (reusing your keys, agent, `~/.ssh/config`, and `known_hosts`). Push and pull
  are both supported; the receiver drives a deadlock-free lock-step exchange and
  every write is containment-checked under the destination root. New `net` module
  (`proto`, `transport`, `sender`, `receiver`, `server`) and a hidden
  `ripsync --server` peer mode.
- **Delta over the wire.** The rolling-checksum delta engine was split into a
  serializable `Signature` (computed by the receiver) and `encode_with_signature`
  (run by the sender), so changed files transfer as deltas instead of whole
  copies. `--whole-file`/`-W` forces whole-file transfer.
- **Wire compression.** `--compress`/`-z` (with `--compress-level`) zstd-compresses
  whole-file payloads.
- **Bandwidth limiting.** `--bwlimit RATE` token-bucket-throttles the upload rate
  (bare number = KiB/s, with `K`/`M`/`G` suffixes), like rsync.
- **Device-tier auto-tuning.** New `tune` module classifies the machine (CPU
  count + RAM) into a `low`/`balanced`/`high` profile that drives worker-thread
  count, copy-buffer size, io_uring queue depth, and zstd level. Select with
  `--profile {auto,low,balanced,high}`; the active profile shows in the TUI header.
- **ARM64 Windows** (`aarch64-pc-windows-msvc`) added to the release matrix, built
  with the system allocator.

### Changed

- `--bwlimit` and `--partial` are no longer hard errors; `--bwlimit` throttles
  remote transfers and `--partial` is accepted (writes are always atomic).
- The TUI header now reports the active performance profile and the real version.

## 0.4.0 - 2026-06-12

### Added

- **Windows support.** New `io/windows.rs` backend: ReFS / Dev Drive block clone
  (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`) → `CopyFileExW` → buffered, with atomic
  replace via `MoveFileExW`. POSIX-only metadata (mode/uid/gid/xattr/ACL) becomes
  a warn-once no-op; mtime is preserved. Symlinks degrade to a warn-once skip
  when Windows privilege is missing. CI now covers `windows-latest`.
- Opt-in `rs` short-alias binary (`--features rs-alias`), off by default.
- Hidden `ripsync _gen <man|completions> [shell]` and a `cargo xtask dist-assets`
  task generating `ripsync.1` and bash/zsh/fish/powershell completions.
- crates.io metadata on both crates; `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue
  and PR templates, README badges.
- Packaging: cargo-dist release matrix (shell/powershell/homebrew/msi),
  Debian (`cargo-deb`) + apt repo docs, AUR `ripsync` and `ripsync-bin`, Scoop
  and winget manifests.
- mdBook documentation site with a reproducible VHS demo GIF, deployed to Pages.

### Changed

- The project was renamed **ferry → ripsync** (crates `ripsync-core` /
  `ripsync`, manifest dir `.ripsync`, repo `github.com/beeboyd/ripsync`).
- Performance: `foldhash` for manifest and plan-classification maps; `blake3`
  `update_mmap_rayon` for large-file hashing; `posix_fadvise` readahead on the
  Linux large-file path; release profile `lto = "fat"` + `strip`.
- `--backend auto` now resolves against the planned file set: a many-small-files
  workload on Linux selects `io_uring`; otherwise the portable ladder. Windows
  reports the block-clone backend.

### Fixed

- Release profile no longer sets `panic = "abort"`; `panic = "unwind"` is kept so
  the RAII terminal guard and io_uring / Windows-handle `Drop` cleanups run on
  panic.

## 0.3.0 - 2026-06-12

### Added

- Lifecycle TUI from planning through finalization, with real pause, graceful
  cancellation, delete confirmation, tabs, filtering, navigation, compact
  layouts, and `NO_COLOR`.
- Cloneable cooperative `RunControl` and controlled planning/apply APIs.
- `--verify none|changed|all` with structured mismatch reporting.
- Manifest v3 atomic snapshots and checksummed append-only delta journals.
- JSON status, cancellation, phase timing, backend, and verification fields.
- Architecture, safety, TUI, performance, and contributing documentation.

### Changed

- `--backend auto` is portable-first; io_uring remains explicitly selectable.
- Incremental persistence retains validated skipped records and records only
  changed/deleted entries.
- Filesystem walks collect into vectors and parallel-sort once.

### Fixed

- `--bwlimit` and `--partial` now fail explicitly instead of acting as no-ops.
- Cancellation prevents later phases and manifest updates while preserving
  completed atomic destination changes.

### Not Included

Remote sync, watch mode, config profiles, throttling, and partial resume remain
out of scope.
