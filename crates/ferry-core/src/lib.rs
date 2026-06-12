//! Ferry core: pure sync logic — delta engine, checksums, parallel walk,
//! plan/apply, metadata and symlink-containment safety.
//!
//! No terminal I/O lives here; the CLI crate owns presentation.
#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod error;

pub use error::{Error, Result};
