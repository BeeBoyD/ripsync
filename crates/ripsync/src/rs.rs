//! Opt-in `rs` short-alias binary — identical to `ripsync`, built only with
//! `--features rs-alias` (the name collides with the BSD `rs` reshape tool, so
//! it is never installed by default).

/// Global allocator: mimalloc, matching the `ripsync` binary. Opt out with
/// `--features system-malloc`. Must live in the binary crate root.
#[cfg(not(feature = "system-malloc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    ripsync::main();
}
