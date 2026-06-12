//! Ferry core: pure sync logic — delta engine, checksums, parallel walk,
//! plan/apply, metadata and symlink-containment safety.
//!
//! No terminal I/O lives here; the CLI crate owns presentation.
#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod apply;
pub mod checksum;
pub mod delta;
pub mod error;
pub mod meta;
pub mod plan;
pub mod report;
pub mod walk;

pub use error::{Error, Result};
pub use report::{Event, Reporter, Stats};
