//! Ferry core: pure sync logic — delta engine, checksums, parallel walk,
//! plan/apply, metadata and symlink-containment safety.
//!
//! No terminal I/O lives here; the CLI crate owns presentation.
// The whole crate is `forbid(unsafe_code)` — except when the Linux `io-uring`
// backend is compiled in, where the single `io::uring` module opts into `unsafe`
// (a crate-level `forbid` cannot be locally relaxed, so we drop to `deny`, which
// still rejects `unsafe` everywhere that does not explicitly `allow` it).
#![cfg_attr(
    not(all(target_os = "linux", feature = "io-uring")),
    forbid(unsafe_code)
)]
#![cfg_attr(all(target_os = "linux", feature = "io-uring"), deny(unsafe_code))]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod apply;
pub mod checksum;
pub mod copy;
pub mod delta;
pub mod error;
pub mod index;
pub mod io;
pub mod meta;
pub mod plan;
pub mod report;
pub mod walk;

pub use error::{Error, Result};
pub use report::{Event, Reporter, Stats};
