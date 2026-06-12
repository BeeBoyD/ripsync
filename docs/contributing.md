# Contributing

Use Rust 1.85 or newer. Before submitting a change, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --no-default-features
cargo test -p ripsync --features system-malloc
cargo build --workspace --release
```

Changes to planning, apply, verification, containment, or the manifest need
focused failure/cancellation tests. TUI changes should render with Ratatui's
`TestBackend` at compact, normal, and wide dimensions.

Do not add unsafe code. The only accepted unsafe blocks are the two reviewed
SQE submissions in `crates/ripsync-core/src/io/uring.rs`. Benchmark changes must
retain raw data and document conditions; targets are not results.
