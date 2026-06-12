//! Checksums for the delta engine.
//!
//! Two layers, exactly as in the rsync algorithm:
//!
//! * a **weak rolling checksum** ([`RollingChecksum`]) that is cheap to compute
//!   and `O(1)` to slide one byte along a buffer, used to *cheaply reject*
//!   non-matching windows; and
//! * a **strong hash** ([`strong_hash`]) — the first 16 bytes of a BLAKE3 digest —
//!   used to *confirm* a candidate match found via the weak checksum.
//!
//! # Rolling checksum definition
//!
//! With modulus `M = 65536` and a window of length `L`:
//!
//! ```text
//! a = (Σ_k          bytes[k]) mod M
//! b = (Σ_k (L - k) * bytes[k]) mod M       // position-weighted, k = 0..L
//! checksum = a + b * M
//! ```
//!
//! Because `M = 2^16`, reducing modulo `M` is the same as masking the low 16
//! bits, and ordinary wrapping (`mod 2^32`) arithmetic is congruent modulo `M`.
//! We therefore accumulate with wrapping `u32` ops and mask only when reading the
//! value out — this keeps rolling branch-free and exact.

/// Modulus used by the weak checksum (`2^16`).
const M: u32 = 1 << 16;

/// Length of the strong hash prefix kept per block, in bytes.
pub const STRONG_LEN: usize = 16;

/// A 16-byte strong hash (BLAKE3 prefix).
pub type StrongHash = [u8; STRONG_LEN];

/// Strong hash of `data`: the first [`STRONG_LEN`] bytes of its BLAKE3 digest.
#[must_use]
pub fn strong_hash(data: &[u8]) -> StrongHash {
    let digest = blake3::hash(data);
    let mut out = [0u8; STRONG_LEN];
    out.copy_from_slice(&digest.as_bytes()[..STRONG_LEN]);
    out
}

/// Rolling weak checksum over a fixed-length window.
///
/// Construct from a window with [`RollingChecksum::new`], read the current value
/// with [`RollingChecksum::value`], and slide forward one byte at a time with
/// [`RollingChecksum::roll`].
#[derive(Debug, Clone, Copy)]
pub struct RollingChecksum {
    a: u32,
    b: u32,
    window_len: u32,
}

impl RollingChecksum {
    /// Compute the checksum freshly over `window`.
    ///
    /// The window length is fixed for the life of this value; [`roll`] keeps it
    /// constant.
    ///
    /// [`roll`]: RollingChecksum::roll
    #[must_use]
    pub fn new(window: &[u8]) -> Self {
        let len = window.len();
        let window_len = u32::try_from(len).unwrap_or(u32::MAX);
        let mut a: u32 = 0;
        let mut b: u32 = 0;
        for (k, &byte) in window.iter().enumerate() {
            let weight = window_len.wrapping_sub(u32::try_from(k).unwrap_or(0));
            a = a.wrapping_add(u32::from(byte));
            b = b.wrapping_add(weight.wrapping_mul(u32::from(byte)));
        }
        Self { a, b, window_len }
    }

    /// Slide the window one byte to the right.
    ///
    /// `out_byte` is the byte leaving the window on the left; `in_byte` is the
    /// byte entering on the right. The window length is preserved.
    pub fn roll(&mut self, out_byte: u8, in_byte: u8) {
        let out = u32::from(out_byte);
        let in_ = u32::from(in_byte);
        self.a = self.a.wrapping_sub(out).wrapping_add(in_);
        self.b = self
            .b
            .wrapping_sub(self.window_len.wrapping_mul(out))
            .wrapping_add(self.a);
    }

    /// The current checksum value, `a + b * M`, with `a` and `b` reduced mod `M`.
    #[must_use]
    pub fn value(&self) -> u32 {
        (self.a & (M - 1)) | ((self.b & (M - 1)) << 16)
    }

    /// The fixed window length this checksum tracks.
    #[must_use]
    pub fn window_len(&self) -> u32 {
        self.window_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_matches_manual() {
        // a = 1+2+3 = 6 ; b = 3*1 + 2*2 + 1*3 = 10 ; checksum = 6 + 10*65536.
        let rc = RollingChecksum::new(&[1, 2, 3]);
        assert_eq!(rc.value(), 6 + 10 * M);
    }

    #[test]
    fn roll_equals_recompute() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let w = 7usize;
        let mut rc = RollingChecksum::new(&data[..w]);
        for i in 1..=(data.len() - w) {
            rc.roll(data[i - 1], data[i + w - 1]);
            let fresh = RollingChecksum::new(&data[i..i + w]);
            assert_eq!(rc.value(), fresh.value(), "mismatch at offset {i}");
        }
    }

    #[test]
    fn strong_hash_is_stable_and_16_bytes() {
        let h1 = strong_hash(b"hello");
        let h2 = strong_hash(b"hello");
        let h3 = strong_hash(b"world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 16);
    }
}
