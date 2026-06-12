# Verification

ripsync can verify the destination after copying, independently of the durability
(`--fsync`) settings.

| Mode | Flag | What it checks |
|---|---|---|
| none | `--verify none` (default) | nothing beyond the copy itself |
| changed | `--verify changed` | re-hashes every file ripsync copied/updated and compares source vs destination |
| all | `--verify all` | walks the whole destination and compares structure, content, mode, mtime (±1s), and symlink targets against the source — also flags destination-only entries |

Verification runs before the persistent index is written, so a mismatch fails the
run and the manifest is **not** updated — the next run re-detects the difference
rather than trusting a bad snapshot.

Hashing uses BLAKE3. Files at or above 16 MiB are hashed with a memory-mapped,
rayon-parallel pass (`update_mmap_rayon`); smaller files stream through a single
buffer. The same path backs `--checksum`, which classifies files by content hash
instead of size + mtime during planning.

```sh
# Fail the run (exit non-zero) if any copied file does not match the source.
ripsync SRC DST --no-tui --verify changed

# Full-tree audit, including extra files in the destination.
ripsync SRC DST --no-tui --verify all
```

In the TUI, verification can be triggered and watched interactively; see the
[TUI workflow](tui.md).
