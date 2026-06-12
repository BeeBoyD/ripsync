//! Low-level I/O backends.
//!
//! The portable path (reflink / `copy_file_range` / buffered) lives in
//! [`crate::copy`]. On Linux an optional [`uring`] backend batches many
//! small-file copies through a single `io_uring` to cut syscall overhead.

#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub mod uring;
