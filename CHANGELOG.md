# Changelog

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
