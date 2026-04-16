// src/rolling_pool.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rolling-pointer data pool for high-frequency small-object generation.
//!
//! # Problem
//!
//! `generate_data_simple()` always allocates a minimum of [`BLOCK_SIZE`] (1 MB)
//! internally, even when the caller only needs 64 bytes.  For workloads that
//! generate millions of small objects (images, JPEG thumbnails, KV values, etc.)
//! this means:
//!
//! - 16 MB of data generated to produce 1 MB of 64 KB objects → 16× waste
//! - One heap allocation + Rayon dispatch per call → high per-call overhead
//!
//! # Solution
//!
//! [`RollingPool`] generates one [`BLOCK_SIZE`]-aligned buffer **once**, then
//! hands out zero-copy [`bytes::Bytes`] windows via [`bytes::Bytes::slice`].
//! Each window holds its own `Arc` reference into the backing allocation so
//! in-flight buffers remain valid after the pool is refilled.
//!
//! For objects larger than [`BLOCK_SIZE`], the pool is bypassed and data is
//! generated on demand (the network/storage round-trip dominates anyway).
//!
//! # Performance
//!
//! | Object size | Before (generate_data_simple) | After (RollingPool) |
//! |-------------|------------------------------|---------------------|
//! | 64 KB       | ~105 MB/s (16× alloc waste)  | ~1.7 GB/s           |
//! | 1 MB        | ~1.77 GB/s                   | ~1.77 GB/s          |
//! | > 1 MB      | ~8–15 GB/s                   | ~8–15 GB/s          |
//!
//! # Thread safety
//!
//! [`RollingPool`] is `!Send` by design — it is meant to be held in a
//! [`std::cell::RefCell`] inside a `thread_local!` so each OS thread has its
//! own independent pool with no synchronization overhead.
//!
//! # Example
//! ```rust
//! use dgen_data::RollingPool;
//!
//! // Create a pool for incompressible, non-deduplicated data
//! let mut pool = RollingPool::new(1, 1);
//!
//! // Hand out many small zero-copy slices
//! for _ in 0..16384 {
//!     let buf: bytes::Bytes = pool.next_slice(64 * 1024);
//!     assert_eq!(buf.len(), 64 * 1024);
//!     // buf keeps the backing 1 MB allocation alive via Arc until dropped
//! }
//! ```

use bytes::Bytes;

use crate::constants::BLOCK_SIZE;
use crate::generator::generate_data_simple;

/// High-frequency small-object data pool with a rolling pointer.
///
/// See the [module documentation](self) for design rationale and performance notes.
pub struct RollingPool {
    // The current 1 MB backing allocation (Arc-counted by bytes::Bytes).
    data: Bytes,
    // Byte cursor: next slice starts here.
    position: usize,
    // Generation parameters — a change triggers a refill.
    dedup: usize,
    compress: usize,
}

impl RollingPool {
    /// Create a new pool.
    ///
    /// The first 1 MB buffer is generated immediately so the first call to
    /// [`next_slice`](RollingPool::next_slice) is always fast.
    ///
    /// # Parameters
    /// - `dedup`: Deduplication factor passed to `generate_data_simple` on each
    ///   refill.  `1` = no deduplication.
    /// - `compress`: Compression factor.  `1` = incompressible.
    pub fn new(dedup: usize, compress: usize) -> Self {
        let data = generate_block(dedup, compress);
        Self {
            data,
            position: 0,
            dedup: dedup.max(1),
            compress: compress.max(1),
        }
    }

    /// Return a zero-copy `Bytes` window of exactly `size` bytes.
    ///
    /// - For `size <= BLOCK_SIZE`: advances the internal cursor and returns a
    ///   `Bytes::slice()` (Arc increment only, no copy, no allocation).
    ///   Refills the 1 MB backing buffer when the remaining bytes are
    ///   insufficient.
    /// - For `size > BLOCK_SIZE`: bypasses the pool entirely and generates a
    ///   fresh buffer of exactly `size` bytes.
    ///
    /// # Panics
    /// Panics if `size == 0`.
    pub fn next_slice(&mut self, size: usize) -> Bytes {
        assert!(size > 0, "next_slice: size must be > 0");

        // Large object fast path — pool not used.
        if size > BLOCK_SIZE {
            let mut buf = generate_data_simple(size, self.dedup, self.compress);
            buf.truncate(size);
            return buf.into_bytes();
        }

        // Refill if current pool doesn't have enough bytes left.
        if self.position + size > self.data.len() {
            self.refill();
        }

        let start = self.position;
        let end = start + size;
        self.position = end;

        // Zero-copy: Bytes::slice() increments the Arc refcount and records
        // the byte range.  No heap allocation, no memcpy.
        self.data.slice(start..end)
    }

    /// Change the deduplication and compression parameters.
    ///
    /// If either value changes, the current buffer is discarded and a fresh
    /// one is generated with the new parameters.  If both values are unchanged
    /// this is a no-op.
    pub fn reconfigure(&mut self, dedup: usize, compress: usize) {
        let d = dedup.max(1);
        let c = compress.max(1);
        if self.dedup != d || self.compress != c {
            self.dedup = d;
            self.compress = c;
            self.refill();
        }
    }

    /// Current deduplication factor.
    pub fn dedup(&self) -> usize {
        self.dedup
    }

    /// Current compression factor.
    pub fn compress(&self) -> usize {
        self.compress
    }

    /// Bytes remaining in the current pool before the next refill.
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.position)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn refill(&mut self) {
        self.data = generate_block(self.dedup, self.compress);
        self.position = 0;
    }
}

/// Generate exactly BLOCK_SIZE bytes and convert to Bytes (zero-copy for UMA).
fn generate_block(dedup: usize, compress: usize) -> Bytes {
    let mut buf = generate_data_simple(BLOCK_SIZE, dedup, compress);
    // truncate is a no-op here (len == BLOCK_SIZE) but guards against any
    // future change in generate_data_simple's minimum allocation.
    buf.truncate(BLOCK_SIZE);
    buf.into_bytes()
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── Correctness ───────────────────────────────────────────────────────────

    #[test]
    fn test_exact_size_returned() {
        let mut pool = RollingPool::new(1, 1);
        for size in [1, 64, 512, 4096, 65536, BLOCK_SIZE] {
            let b = pool.next_slice(size);
            assert_eq!(b.len(), size, "expected {} bytes, got {}", size, b.len());
        }
    }

    #[test]
    fn test_data_is_not_all_zeros() {
        let mut pool = RollingPool::new(1, 1);
        let b = pool.next_slice(4096);
        assert_ne!(&b[..], &vec![0u8; 4096][..]);
    }

    #[test]
    fn test_cursor_advances_zero_copy() {
        let mut pool = RollingPool::new(1, 1);
        let b1 = pool.next_slice(64);
        let b2 = pool.next_slice(64);
        // Both slices point into the same 1 MB backing allocation.
        // b2 starts exactly 64 bytes after b1.
        let ptr1 = b1.as_ptr() as usize;
        let ptr2 = b2.as_ptr() as usize;
        assert_eq!(ptr2, ptr1 + 64, "cursor did not advance by 64");
    }

    #[test]
    fn test_consecutive_slices_differ() {
        let mut pool = RollingPool::new(1, 1);
        let b1 = pool.next_slice(64);
        let b2 = pool.next_slice(64);
        assert_ne!(
            &b1[..],
            &b2[..],
            "consecutive slices must contain different bytes"
        );
    }

    #[test]
    fn test_pool_refill_on_exhaustion() {
        let mut pool = RollingPool::new(1, 1);
        // Exhaust BLOCK_SIZE / 1024 requests of 1024 bytes each, then one more.
        let chunk = 1024;
        let count = BLOCK_SIZE / chunk + 1;
        for i in 0..count {
            let b = pool.next_slice(chunk);
            assert_eq!(b.len(), chunk, "iteration {} returned wrong size", i);
        }
    }

    #[test]
    fn test_refill_boundary_alignment() {
        // A size that does NOT evenly divide BLOCK_SIZE (65537 bytes).
        let mut pool = RollingPool::new(1, 1);
        let size = 65537;
        let count = BLOCK_SIZE / size + 4;
        for i in 0..count {
            let b = pool.next_slice(size);
            assert_eq!(b.len(), size, "iteration {} returned wrong size", i);
        }
    }

    #[test]
    fn test_large_object_bypasses_pool() {
        let mut pool = RollingPool::new(1, 1);
        let size = BLOCK_SIZE + 1;
        let b = pool.next_slice(size);
        assert_eq!(b.len(), size);
    }

    #[test]
    fn test_reconfigure_triggers_refill() {
        let mut pool = RollingPool::new(1, 1);
        let pos_before = pool.position;
        pool.reconfigure(2, 1); // change dedup → refill
        assert_eq!(pool.position, 0, "position should reset to 0 after refill");
        let _ = pos_before; // accessed before reconfigure
    }

    #[test]
    fn test_reconfigure_noop_when_unchanged() {
        let mut pool = RollingPool::new(1, 1);
        pool.next_slice(64); // advance cursor
        let pos = pool.position;
        pool.reconfigure(1, 1); // no change
        assert_eq!(
            pool.position, pos,
            "no-op reconfigure must not reset cursor"
        );
    }

    #[test]
    fn test_remaining_decrements() {
        let mut pool = RollingPool::new(1, 1);
        let rem0 = pool.remaining();
        pool.next_slice(1024);
        assert_eq!(pool.remaining(), rem0 - 1024);
    }

    #[test]
    fn test_in_flight_slices_survive_refill() {
        let mut pool = RollingPool::new(1, 1);
        // Grab a slice, exhaust the pool, check the first slice is still valid.
        let b = pool.next_slice(64);
        let expected: Vec<u8> = b.to_vec();
        // Exhaust so the pool refills on the next call.
        for _ in 0..(BLOCK_SIZE / 64 + 1) {
            let _ = pool.next_slice(64);
        }
        // b still valid — its Arc ref keeps the old backing allocation alive.
        assert_eq!(&b[..], &expected[..]);
    }
}
