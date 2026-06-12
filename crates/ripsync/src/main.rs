//! ripsync CLI entry point — a thin shell over the library [`ripsync::main`].

/// Global allocator: mimalloc speeds up the alloc-heavy parallel walk. Opt out
/// with `--features system-malloc` (or `--no-default-features`). Must live in
/// the binary crate root.
#[cfg(all(feature = "mimalloc", not(feature = "system-malloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    ripsync::main();
}
