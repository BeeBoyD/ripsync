# Safety

## Writes

Regular files are copied to `.ripsync-tmp-*` in the target directory, metadata is
applied, and the file is atomically renamed. Copy failures remove the temporary
file. Destination paths and symlink ancestors are checked against the
canonical destination root.

## Cancellation

Pause and cancellation are cooperative. Work already inside one atomic
operation or bounded parallel chunk finishes. Checkpoints prevent subsequent
chunks and phases from starting. A cancelled run:

- retains destination operations already completed;
- leaves no ripsync temporary files;
- performs no later verification or finalization;
- does not update the manifest;
- exits with status `130`.

Partial resume is not implemented.

## Deletion

An empty or unreadable source cannot drive deletion. Interactive TUI deletion
requires typing `DELETE`; `--yes` is required for noninteractive automation.
Deletes run deepest-first and use the same containment checks as writes.

## Verification

`changed` hashes copied/updated files and checks touched metadata and links.
`all` additionally compares complete path sets. File kinds, modes, mtimes,
symlink targets, and BLAKE3 content hashes are checked. Hardlinks, sparse
allocation, xattrs, ACLs, uid, and gid are checked only when their preservation
flags are enabled. `.ripsync` is excluded.

Mismatches are reported structurally, prevent manifest persistence, and return
exit status `1`.

## Exit Codes

| Code | Meaning |
|---|---|
| `0` | Successful run |
| `1` | Planning, apply, verification, or configuration failure |
| `2` | Clap argument syntax error |
| `130` | Graceful user cancellation |
