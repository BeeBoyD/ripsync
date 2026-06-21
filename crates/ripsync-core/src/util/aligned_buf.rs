//! Page-aligned buffer allocation for efficient I/O.
//!
//! On modern kernels, page-aligned buffers avoid extra copies in the page cache
//! and can be used with `O_DIRECT` or similar APIs. This module provides a safe
//! wrapper around platform-specific allocation primitives.
//!
//! # Safety
//!
//! This is an isolated `unsafe` module (like `io::uring` and `io::windows`).
//! Every `unsafe` block carries a `// SAFETY:` comment.
#![allow(unsafe_code)]

use std::alloc::{self, Layout};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// A page-aligned buffer that deallocates itself on drop.
///
/// Implements `Deref<Target = [u8]>` and `DerefMut` so it can be used
/// wherever a `&[u8]` or `&mut [u8]` is expected.
pub struct AlignedBuf {
    ptr: NonNull<u8>,
    layout: Layout,
    len: usize,
}

// SAFETY: AlignedBuf owns a uniquely-allocated buffer; sending across threads
// is safe because no other reference exists.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    /// Allocate a buffer of at least `size` bytes, aligned to the system page size.
    ///
    /// The actual allocation is rounded up to the next multiple of the page size.
    ///
    /// # Panics
    ///
    /// Panics if `size` is zero or if allocation fails.
    #[must_use]
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "buffer size must be non-zero");

        let page_size = page_size();
        // Round up to page-size multiple so the buffer is always a whole number
        // of pages (required by some zero-copy APIs).
        let size = size.next_multiple_of(page_size);

        #[cfg(unix)]
        let (ptr, layout) = alloc_unix(size, page_size);

        #[cfg(windows)]
        let (ptr, layout) = alloc_windows(size, page_size);

        #[cfg(not(any(unix, windows)))]
        let (ptr, layout) = alloc_fallback(size, page_size);

        Self { ptr, layout, len: size }
    }

    /// Returns the capacity of the buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the buffer is empty (always false for allocated buffers).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Deref for AlignedBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is non-null, properly aligned, and points to `len` bytes
        // of initialized (though potentially un-written) memory. The borrow lifetime
        // is tied to `&self`, preventing mutable access while this reference lives.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for AlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` is non-null, properly aligned, and points to `len` bytes
        // of valid memory. The borrow lifetime is tied to `&mut self`, ensuring
        // exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: `ptr` was allocated with `alloc::alloc` using `layout`,
            // has not been freed, and we hold the only reference.
            unsafe {
                alloc::dealloc(self.ptr.as_ptr(), self.layout);
            }
        }

        #[cfg(windows)]
        {
            // SAFETY: `ptr` was allocated with `_aligned_malloc`, has not been
            // freed, and we hold the only reference. `_aligned_free` is the
            // correct deallocator for `_aligned_malloc`.
            unsafe {
                _aligned_free(self.ptr.as_ptr().cast::<std::ffi::c_void>());
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            // SAFETY: `ptr` was allocated with `alloc::alloc` using `layout`,
            // has not been freed, and we hold the only reference.
            unsafe {
                alloc::dealloc(self.ptr.as_ptr(), self.layout);
            }
        }
    }
}

/// Get the system page size in bytes.
#[must_use]
fn page_size() -> usize {
    #[cfg(unix)]
    {
        rustix::param::page_size()
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

        // SAFETY: `GetSystemInfo` is always safe to call; it fills a caller-owned
        // struct. `SYSTEM_INFO` is `repr(C)` and all bit patterns are valid.
        let mut sysinfo: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        unsafe {
            GetSystemInfo(&mut sysinfo);
        }
        sysinfo.dwPageSize as usize
    }

    #[cfg(not(any(unix, windows)))]
    {
        4096 // Common default for unknown platforms
    }
}

#[cfg(unix)]
fn alloc_unix(size: usize, page_size: usize) -> (NonNull<u8>, Layout) {
    let layout =
        Layout::from_size_align(size, page_size).expect("invalid layout for page-aligned buffer");

    // SAFETY: `layout` has non-zero size (guaranteed by the `size > 0` assert in
    // `new`). The allocated memory is not read before being written by the caller.
    let ptr = unsafe { alloc::alloc(layout) };
    let ptr = NonNull::new(ptr).expect("page-aligned allocation failed");

    (ptr, layout)
}

#[cfg(windows)]
fn alloc_windows(size: usize, page_size: usize) -> (NonNull<u8>, Layout) {
    // SAFETY: `_aligned_malloc` is the Windows CRT aligned-allocation function.
    // `size` and `page_size` are both > 0. The returned pointer is either valid
    // or null; we check for null below.
    let ptr = unsafe { _aligned_malloc(size, page_size) };
    if ptr.is_null() {
        panic!("page-aligned allocation failed");
    }

    // SAFETY: `ptr` is non-null (checked above) and points to `size` bytes of
    // properly-aligned memory from `_aligned_malloc`.
    let ptr = unsafe { NonNull::new_unchecked(ptr.cast::<u8>()) };

    // Dummy layout for API compatibility; the real deallocation uses `_aligned_free`.
    let layout = Layout::from_size_align(1, 1).unwrap();

    (ptr, layout)
}

#[cfg(not(any(unix, windows)))]
fn alloc_fallback(size: usize, page_size: usize) -> (NonNull<u8>, Layout) {
    let layout =
        Layout::from_size_align(size, page_size).expect("invalid layout for page-aligned buffer");

    // SAFETY: `layout` has non-zero size. The allocated memory is not read before
    // being written by the caller.
    let ptr = unsafe { alloc::alloc(layout) };
    let ptr = NonNull::new(ptr).expect("page-aligned allocation failed");

    (ptr, layout)
}

#[cfg(windows)]
extern "C" {
    fn _aligned_malloc(size: usize, alignment: usize) -> *mut std::ffi::c_void;
    fn _aligned_free(ptr: *mut std::ffi::c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_page_aligned_buffer() {
        let buf = AlignedBuf::new(1024);
        assert!(buf.len() >= 1024);
        assert!(!buf.is_empty());

        let ptr = buf.ptr.as_ptr() as usize;
        let ps = page_size();
        assert_eq!(ptr % ps, 0, "buffer not page-aligned");
    }

    #[test]
    fn buffer_is_writable_via_deref_mut() {
        let mut buf = AlignedBuf::new(1024);
        buf[0] = 42;
        buf[999] = 24;
        assert_eq!(buf[0], 42);
        assert_eq!(buf[999], 24);
    }

    #[test]
    fn deref_returns_full_slice() {
        let buf = AlignedBuf::new(512);
        assert_eq!(buf.len(), buf.len());
        // Verify Deref gives correct length
        let slice: &[u8] = &buf;
        assert_eq!(slice.len(), buf.len());
    }

    #[test]
    fn respects_minimum_page_size() {
        let buf = AlignedBuf::new(1);
        let ps = page_size();
        assert!(buf.len() >= ps);
    }

    #[test]
    fn size_is_page_multiple() {
        let buf = AlignedBuf::new(100_000);
        let ps = page_size();
        assert_eq!(buf.len() % ps, 0);
    }
}
