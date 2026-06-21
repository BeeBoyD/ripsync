//! Low-level I/O backends.
//!
//! The portable path (reflink / `copy_file_range` / buffered) lives in
//! [`crate::copy`]. On Linux an optional [`uring`] backend batches many
//! small-file copies through a single `io_uring` to cut syscall overhead. On
//! Windows [`windows`] provides `ReFS` block-clone, `CopyFileExW`, and atomic
//! replace via `MoveFileExW`.

#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub mod uring;

#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub use uring::kernel_supports_splice;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(windows)]
pub mod windows;
