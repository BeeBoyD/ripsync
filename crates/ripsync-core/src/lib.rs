//! ripsync core: pure sync logic — delta engine, checksums, parallel walk,
//! plan/apply, metadata and symlink-containment safety.
//!
//! No terminal I/O lives here; the CLI crate owns presentation.
// The whole crate is `forbid(unsafe_code)` — except in the isolated platform IO
// modules: the Linux `io::uring` backend and the Windows `io::windows` backend,
// which must call raw OS APIs. A crate-level `forbid` cannot be locally relaxed,
// so on those configurations we drop to `deny`, which still rejects `unsafe`
// everywhere that does not explicitly `allow` it. Every `unsafe` block in those
// modules carries a `// SAFETY:` comment.
#![cfg_attr(
    not(any(all(target_os = "linux", feature = "io-uring"), windows)),
    forbid(unsafe_code)
)]
#![cfg_attr(
    any(all(target_os = "linux", feature = "io-uring"), windows),
    deny(unsafe_code)
)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod apply;
pub mod checksum;
pub mod control;
pub mod copy;
pub mod delta;
pub mod error;
pub mod index;
pub mod io;
pub mod meta;
pub mod plan;
pub mod report;
pub mod verify;
pub mod walk;

pub use control::RunControl;
pub use error::{Error, Result};
pub use report::{Event, Reporter, RunPhase, RunStatus, Stats};
