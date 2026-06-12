# ripsync

> rsync's superpower, none of its footguns.

ripsync is a fast, memory-safe local directory synchronization tool written in
Rust. It mirrors a source tree into a destination using copy-on-write reflinks
where available, a persistent index for quick re-syncs, an operator TUI with
pause/cancel/verify, and optional post-copy verification.

![ripsync TUI demo](assets/ripsync-demo.gif)

## Why ripsync

- **Memory-safe by construction.** The engine is `#![forbid(unsafe_code)]` apart
  from two isolated, audited platform IO modules (Linux `io_uring`, Windows
  ReFS). This rules out the buffer-overflow / use-after-free CVE classes seen in
  C file-transfer tools.
- **No symlink footgun.** Every write, link, and delete target is checked for
  destination containment first, rejecting the rsync symlink path-traversal bug
  class (CVE-2024-12087/12088) by design. See the [safety model](safety.md).
- **Fast.** Parallel walk and copy, reflink / `copy_file_range` / `io_uring` on
  Linux, ReFS block clone / `CopyFileExW` on Windows, `clonefile` on macOS, a
  persistent index for incremental re-syncs, and `foldhash` + `blake3` mmap
  hashing. See [backends & performance](performance.md).
- **Operator-friendly.** A TUI to pause, cancel, filter, and verify, plus
  `--dry-run`, guarded `--delete`, and `--stats`/JSON for automation.

## Scope

ripsync syncs **local** directories. Remote/network sync and watch mode are not
implemented and are out of scope for the 0.x line.
