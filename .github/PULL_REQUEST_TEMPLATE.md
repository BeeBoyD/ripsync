<!-- Thanks for contributing to ripsync! -->

## Summary

<!-- What does this change do, and why? -->

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` is clean
- [ ] `cargo test --all --all-features` passes
- [ ] If this touches apply/copy, I re-ran the parity-vs-rsync harness
- [ ] `ripsync-core` stays `#![forbid(unsafe_code)]` outside `io/uring.rs` and
      `io/windows.rs`; any new `unsafe` block has a `// SAFETY:` comment
- [ ] Docs / CHANGELOG updated if behavior changed

## Notes

<!-- Anything reviewers should know: trade-offs, follow-ups, benchmark deltas. -->
