# Changelog

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
