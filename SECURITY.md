# Security Policy

## Memory-safety posture

ripsync is written in safe Rust. The core engine (`ripsync-core`) is
`#![forbid(unsafe_code)]` everywhere except two isolated, audited platform IO
modules:

- `io/uring.rs` — the Linux `io_uring` batched-copy backend;
- `io/windows.rs` — the Windows ReFS block-clone / `CopyFileExW` backend.

Those modules drop to `#![deny(unsafe_code)]` and annotate every `unsafe` block
with a `// SAFETY:` rationale. The CLI crate adds no `unsafe`. This rules out the
memory-corruption bug classes (buffer overflows, use-after-free) that have
produced CVEs in C-based file-transfer tools.

## The rsync CVE class ripsync rejects by construction

rsync has repeatedly shipped **symlink path-traversal** vulnerabilities
(e.g. CVE-2024-12087 / CVE-2024-12088), where a malicious source tree redirects
a write outside the intended destination through a symlink.

ripsync refuses this by design: before writing a file, creating a symlink, or
deleting anything, the *real* parent directory of every target is canonicalized
and checked to lie inside the destination root. A pre-existing symlink that
redirects a write outside the root is refused, never followed. Source-relative
paths containing `..`, a root, or a drive prefix are rejected before any join.
These checks hold on every backend and OS, and are covered by the parity and
containment tests.

## Supported versions

The latest released `0.x` minor version receives security fixes.

## Reporting a vulnerability

Please report security issues privately using GitHub's
[private vulnerability reporting](https://github.com/beeboyd/ripsync/security/advisories/new)
for this repository, or by email to **ionescudan888@gmail.com**.

Do not open a public issue for an undisclosed vulnerability. We aim to
acknowledge a report within 72 hours and to ship a fix or mitigation for
confirmed issues as quickly as is practical, crediting reporters who wish it.
