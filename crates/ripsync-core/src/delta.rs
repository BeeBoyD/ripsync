//! The delta engine — ripsync's crown jewel.
//!
//! Given an `old` buffer and a `new` buffer, [`encode`] produces a compact
//! [`Delta`]: a sequence of [`Op::Copy`] (reuse a block of `old`) and
//! [`Op::Literal`] (raw bytes that aren't in `old`) operations. [`apply`] stitches
//! those back together so that, for all inputs,
//!
//! ```text
//! apply(old, encode(old, new, _)) == new
//! ```
//!
//! ## Algorithm
//!
//! 1. Choose a block size `B = clamp(round(sqrt(old.len())), 700, 131072)` (or an
//!    explicit override).
//! 2. Split `old` into blocks of `B` bytes (the last may be short). For each block
//!    record its weak rolling checksum and strong hash, keyed weak → block indices.
//! 3. Roll a `B`-byte window across `new`. When the weak checksum hits a known
//!    block *and* the strong hash confirms it, flush any pending literal run, emit
//!    a [`Op::Copy`], and jump the window forward by `B`. Otherwise push the
//!    leftmost byte into the pending literal buffer and slide the window by one.
//! 4. A short trailing block of `old` is matched against the remaining tail of
//!    `new`, so an unchanged file re-encodes to pure copies with zero literals.

use std::collections::HashMap;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::checksum::{RollingChecksum, StrongHash, strong_hash};

/// Minimum auto-selected block size, in bytes.
pub const MIN_BLOCK: usize = 700;
/// Maximum auto-selected block size, in bytes.
pub const MAX_BLOCK: usize = 131_072;

/// A single delta operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// Reuse block `index` of `old` (its byte range is `index*B .. index*B + len`).
    Copy(u32),
    /// Insert these raw bytes verbatim.
    Literal(Vec<u8>),
}

/// A delta that reconstructs `new` from `old`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    /// Block size used when this delta was produced.
    pub block_size: u32,
    /// The operations, in order.
    pub ops: Vec<Op>,
}

impl Delta {
    /// Number of bytes carried as literals (i.e. not reused from `old`).
    #[must_use]
    pub fn literal_bytes(&self) -> usize {
        self.ops
            .iter()
            .map(|op| match op {
                Op::Literal(b) => b.len(),
                Op::Copy(_) => 0,
            })
            .sum()
    }

    /// Number of [`Op::Copy`] operations.
    #[must_use]
    pub fn copy_ops(&self) -> usize {
        self.ops.iter().filter(|o| matches!(o, Op::Copy(_))).count()
    }
}

/// Choose a block size for `old_len`: `clamp(round(sqrt(old_len)), 700, 131072)`.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
pub fn block_size_for(old_len: usize) -> usize {
    let approx = (old_len as f64).sqrt().round() as usize;
    approx.clamp(MIN_BLOCK, MAX_BLOCK)
}

/// One block's fingerprint inside a [`Signature`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSig {
    /// Weak rolling checksum of the block.
    pub weak: u32,
    /// Strong hash (BLAKE3 prefix) of the block.
    pub strong: StrongHash,
    /// Block length in bytes (all `block_size` except possibly the last).
    pub len: u32,
}

/// A compact fingerprint of an `old` buffer: everything the delta encoder needs
/// to find reusable blocks, and nothing else.
///
/// This is the wire-friendly half of the rsync split. The machine that holds the
/// *old* file ("receiver") computes a [`Signature`] with [`Signature::compute`]
/// and sends it across; the machine that holds the *new* file ("sender") feeds it
/// to [`encode_with_signature`] to produce a [`Delta`] — without ever shipping the
/// old bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// Block size the blocks were cut at.
    pub block_size: u32,
    /// Per-block fingerprints, in `old` order.
    pub blocks: Vec<BlockSig>,
}

impl Signature {
    /// Compute the signature of `old`.
    ///
    /// `block_override` forces a block size (must be ≥ 1); otherwise
    /// [`block_size_for`] picks one from `old.len()`.
    #[must_use]
    pub fn compute(old: &[u8], block_override: Option<usize>) -> Self {
        let block = block_override.map_or_else(|| block_size_for(old.len()), |b| b.max(1));
        let block_size = u32::try_from(block).unwrap_or(u32::MAX);
        let blocks: Vec<BlockSig> = old
            .par_chunks(block)
            .map(|chunk| BlockSig {
                weak: RollingChecksum::new(chunk).value(),
                strong: strong_hash(chunk),
                len: u32::try_from(chunk.len()).unwrap_or(u32::MAX),
            })
            .collect();
        Self { block_size, blocks }
    }

    /// Number of blocks in the signature.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the signature is empty (the `old` buffer had no bytes).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// Acceleration structure built from a [`Signature`] at encode time: weak
/// checksum → indices of blocks sharing it.
struct Lookup<'a> {
    sig: &'a Signature,
    by_weak: HashMap<u32, Vec<u32>>,
}

impl<'a> Lookup<'a> {
    fn new(sig: &'a Signature) -> Self {
        let mut by_weak: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, block) in sig.blocks.iter().enumerate() {
            let idx = u32::try_from(i).unwrap_or(u32::MAX);
            by_weak.entry(block.weak).or_default().push(idx);
        }
        Self { sig, by_weak }
    }

    /// Find a block matching `weak` and the window contents.
    fn find(&self, weak: u32, window: &[u8]) -> Option<u32> {
        let candidates = self.by_weak.get(&weak)?;
        let mut strong: Option<StrongHash> = None;
        for &bi in candidates {
            let block = &self.sig.blocks[bi as usize];
            if block.len as usize != window.len() {
                continue;
            }
            let sh = *strong.get_or_insert_with(|| strong_hash(window));
            if block.strong == sh {
                return Some(bi);
            }
        }
        None
    }
}

/// Encode `new` against a previously computed [`Signature`] of `old`.
///
/// This is the half of the delta engine that runs on the machine holding `new`
/// (the rsync "sender"): it needs only the signature, never `old` itself.
#[must_use]
pub fn encode_with_signature(sig: &Signature, new: &[u8]) -> Delta {
    let block = sig.block_size as usize;
    let block_size = sig.block_size;

    // Degenerate: nothing to copy from.
    if sig.blocks.is_empty() {
        let ops = if new.is_empty() {
            Vec::new()
        } else {
            vec![Op::Literal(new.to_vec())]
        };
        return Delta { block_size, ops };
    }

    let index = Lookup::new(sig);
    let mut ops: Vec<Op> = Vec::new();
    let n = new.len();
    let mut pos = 0usize; // window start
    let mut lit_start = 0usize; // start of pending literal run

    let flush = |ops: &mut Vec<Op>, from: usize, to: usize| {
        if to > from {
            ops.push(Op::Literal(new[from..to].to_vec()));
        }
    };

    if n >= block {
        let mut rc = RollingChecksum::new(&new[..block]);
        loop {
            let window = &new[pos..pos + block];
            if let Some(bi) = index.find(rc.value(), window) {
                flush(&mut ops, lit_start, pos);
                ops.push(Op::Copy(bi));
                pos += block;
                lit_start = pos;
                if pos + block <= n {
                    rc = RollingChecksum::new(&new[pos..pos + block]);
                    continue;
                }
                break;
            }
            if pos + block < n {
                rc.roll(new[pos], new[pos + block]);
                pos += 1;
            } else {
                // Window sits at the very end; can't slide further.
                break;
            }
        }
    }

    // Tail: try to match a short trailing block of `old` against the remainder,
    // then emit whatever is left as a final literal.
    let remaining = &new[lit_start..n];
    if !remaining.is_empty() {
        let weak = RollingChecksum::new(remaining).value();
        if let Some(bi) = index.find(weak, remaining) {
            ops.push(Op::Copy(bi));
        } else {
            ops.push(Op::Literal(remaining.to_vec()));
        }
    }

    Delta { block_size, ops }
}

/// Encode the difference from `old` to `new`.
///
/// Convenience wrapper for callers that hold both buffers locally: computes the
/// signature of `old` and encodes `new` against it. `block_override` forces a
/// block size (must be ≥ 1); otherwise [`block_size_for`] picks one.
#[must_use]
pub fn encode(old: &[u8], new: &[u8], block_override: Option<usize>) -> Delta {
    encode_with_signature(&Signature::compute(old, block_override), new)
}

/// Reconstruct `new` by applying `delta` to `old`.
///
/// # Errors
///
/// Returns [`crate::Error::DeltaApply`] if a [`Op::Copy`] references a block that
/// does not exist in `old` (e.g. a corrupt or mismatched delta).
pub fn apply(old: &[u8], delta: &Delta) -> crate::Result<Vec<u8>> {
    let block = delta.block_size as usize;
    let mut out: Vec<u8> = Vec::new();
    for op in &delta.ops {
        match op {
            Op::Literal(bytes) => out.extend_from_slice(bytes),
            Op::Copy(bi) => {
                let start = (*bi as usize)
                    .checked_mul(block)
                    .ok_or_else(|| crate::Error::DeltaApply("block offset overflow".into()))?;
                if start >= old.len() && !(start == 0 && old.is_empty()) {
                    return Err(crate::Error::DeltaApply(format!(
                        "copy block {bi} out of range"
                    )));
                }
                let end = (start + block).min(old.len());
                out.extend_from_slice(&old[start..end]);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(old: &[u8], new: &[u8], block: Option<usize>) -> Delta {
        let d = encode(old, new, block);
        let rebuilt = apply(old, &d).expect("apply");
        assert_eq!(rebuilt, new, "roundtrip mismatch");
        d
    }

    #[test]
    fn empty_old_empty_new() {
        let d = roundtrip(b"", b"", None);
        assert!(d.ops.is_empty());
    }

    #[test]
    fn empty_old_some_new() {
        let d = roundtrip(b"", b"hello world", None);
        assert_eq!(d.ops, vec![Op::Literal(b"hello world".to_vec())]);
    }

    #[test]
    fn some_old_empty_new() {
        let d = roundtrip(b"abcdef", b"", None);
        assert!(d.ops.is_empty());
    }

    #[test]
    fn identical_is_all_copy_zero_literals() {
        let old: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let d = roundtrip(&old, &old, Some(700));
        assert_eq!(d.literal_bytes(), 0, "expected zero literals");
        assert!(d.copy_ops() > 0);
        assert!(d.ops.iter().all(|o| matches!(o, Op::Copy(_))));
    }

    #[test]
    fn completely_different_is_all_literal() {
        let old = vec![0u8; 4096];
        let new = vec![0xFFu8; 4096];
        let d = roundtrip(&old, &new, Some(700));
        assert_eq!(d.copy_ops(), 0, "expected no copies");
        assert_eq!(d.literal_bytes(), new.len());
    }

    #[test]
    fn new_shorter_than_old() {
        let old: Vec<u8> = (0..3000u32).map(|i| (i % 97) as u8).collect();
        let new = &old[..1234];
        roundtrip(&old, new, Some(700));
    }

    #[test]
    fn new_longer_than_old() {
        let old: Vec<u8> = (0..3000u32).map(|i| (i % 97) as u8).collect();
        let mut new = old.clone();
        new.extend(std::iter::repeat_n(0xAB, 2000));
        let d = roundtrip(&old, &new, Some(700));
        assert!(d.copy_ops() > 0, "prefix should be copied");
    }

    #[test]
    fn single_byte_change_middle() {
        let old: Vec<u8> = (0..8000u32).map(|i| (i % 211) as u8).collect();
        let mut new = old.clone();
        new[4000] ^= 0xFF;
        let d = roundtrip(&old, &new, Some(700));
        // Most blocks unchanged → copies dominate, few literal bytes.
        assert!(d.copy_ops() > 0);
        assert!(
            d.literal_bytes() < 2 * 700,
            "only the changed block should differ"
        );
    }

    #[test]
    fn insertion_at_front_shifts_everything() {
        // Proves the rolling checksum re-syncs after a shift.
        let old: Vec<u8> = (0..8000u32).map(|i| (i % 197) as u8).collect();
        let mut new = vec![1u8, 2, 3, 4, 5];
        new.extend_from_slice(&old);
        let d = roundtrip(&old, &new, Some(700));
        assert!(d.copy_ops() > 0, "shifted blocks must still be found");
        // The 5 inserted bytes plus at most one partial block become literals.
        assert!(d.literal_bytes() <= 5 + 700);
    }

    #[test]
    fn block_size_clamps() {
        assert_eq!(block_size_for(0), MIN_BLOCK);
        assert_eq!(block_size_for(100), MIN_BLOCK);
        assert_eq!(block_size_for(1_000_000), 1000);
        assert_eq!(block_size_for(usize::MAX), MAX_BLOCK);
    }

    #[test]
    fn apply_rejects_out_of_range_copy() {
        let bad = Delta {
            block_size: 4,
            ops: vec![Op::Copy(999)],
        };
        assert!(apply(b"tiny", &bad).is_err());
    }

    #[test]
    fn strong_len_is_16() {
        assert_eq!(crate::checksum::STRONG_LEN, 16);
    }
}
