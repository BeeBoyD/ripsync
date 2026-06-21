//! Utility types and helpers.
//!
//! Currently provides [`AlignedBuf`] — a page-aligned I/O buffer used by the
//! portable copy path to avoid extra page-cache copies.

pub mod aligned_buf;

pub use aligned_buf::AlignedBuf;
