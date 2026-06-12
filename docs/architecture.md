# Architecture

ripsync is a Cargo workspace with a terminal-independent engine in `ripsync-core`
and presentation/argument handling in `ripsync`.

## Run Lifecycle

1. Planning walks the source and either walks the destination or consults the
   v3 index. Indexed entries are still stat-validated against the destination.
2. Review blocks interactive destructive runs until `DELETE` is entered.
3. Apply creates directories, copies files in bounded chunks, restores links
   and metadata, then performs approved deletions.
4. Verification optionally checks changed entries or complete trees.
5. Finalization appends index deltas or writes/compacts a full snapshot.

`RunControl` is shared across these phases. A checkpoint blocks while paused and
returns `Error::Cancelled` after cancellation. Existing `build_plan` and
`apply_plan` APIs remain wrappers around never-cancelled controls.

## Manifest V3

`.ripsync/manifest.bin` is an atomic full snapshot. The first successful sync
writes it using a temporary file and rename. Later successful runs append
records to `.ripsync/manifest.journal`.

Each journal record is:

1. little-endian 32-bit payload length;
2. 32-byte BLAKE3 payload checksum;
3. bincode-encoded upsert or delete payload.

Replay stops at an incomplete, corrupt, or undecodable tail, preserving all
previous complete records. The journal compacts into a new snapshot after
64 MiB or when it exceeds 10% of snapshot size. Old and incompatible formats
are cache misses and trigger a normal destination scan.

## Backends

`auto` selects a per-platform copy backend and reports the choice and reason:

- **Linux/macOS/portable:** the reflink / `copy_file_range` / buffered ladder.
  io_uring is available only through explicit `--backend uring` and falls back
  per file when the ring cannot handle a request. Sparse preservation uses the
  portable path.
- **Windows (`io::windows`):** ReFS / Dev Drive block clone via
  `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, then `CopyFileExW` (which itself block-clones
  on supporting volumes), then a buffered copy. Atomic replace uses `MoveFileExW`
  with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`.

### Platform metadata

POSIX-only metadata (mode, uid/gid, POSIX ACLs, xattrs) is preserved on Unix and
becomes a warn-once no-op on Windows; modification time is always preserved.
Creating symlinks on Windows needs administrator rights or Developer Mode; when
the privilege is missing ripsync warns once and skips the link rather than
failing the run.

The platform IO modules (`io::uring` on Linux, `io::windows` on Windows) are the
only places `ripsync-core` uses `unsafe`; every block carries a `// SAFETY:`
comment and the rest of the crate stays `#![forbid(unsafe_code)]`.
