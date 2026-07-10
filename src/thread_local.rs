// src/thread_local.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Thread-local rolling pool — zero-overhead data generation for async servers.
//!
//! # Problem
//!
//! Every async HTTP server or benchmarking tool that needs to stream fake data
//! ends up writing the same boilerplate:
//!
//! ```rust,ignore
//! use std::cell::RefCell;
//! use dgen_data::RollingPool;
//!
//! thread_local! {
//!     static POOL: RefCell<RollingPool> = RefCell::new(RollingPool::new(1, 1));
//! }
//!
//! fn get_bytes(size: usize) -> bytes::Bytes {
//!     POOL.with(|p| p.borrow_mut().next_slice(size))
//! }
//! ```
//!
//! This module canonicalises that pattern so every project gets it for free.
//!
//! # Design
//!
//! - **One pool per OS/Tokio-worker thread**: no locks, no contention.
//! - **Never re-created**: the pool lives from first use to thread shutdown.
//! - **Never re-seeded between calls**: the rolling pointer advances continuously
//!   across all requests on the thread, so successive calls naturally receive
//!   distinct byte ranges.  Re-seeding only happens when the 1 MiB backing buffer
//!   is exhausted (fresh entropy from `time + urandom`).
//! - **Zero-copy for `size ≤ BLOCK_SIZE`**: each [`next_slice`](crate::thread_local::next_slice) call returns a
//!   [`bytes::Bytes`] Arc slice — no `memcpy`, no heap allocation.
//! - **`Send`-safe with async**: the `POOL.with(...)` borrow is acquired and
//!   released within a single synchronous expression before any `.await` point,
//!   so futures that call [`next_slice`](crate::thread_local::next_slice) are `Send` and can be spawned on a
//!   Tokio multi-thread runtime without any unsafe code.
//!
//! # TODO — verify s3-ultra wires this up before tagging v0.2.4
//!
//! **Before committing and pushing this module**, confirm the status in s3-ultra:
//!
//! - `s3-ultra/src/s3_backend.rs` module doc describes `thread_local::next_slice` as the
//!   canonical GET body generation API, but the **actual implementation** (as of v0.1.6)
//!   still uses a static 32 MiB `OnceLock<Bytes>` pool (introduced in v0.1.5 for the 62×
//!   throughput fix).  The `thread_local` approach was the intended design but was never
//!   wired up.
//! - Decide whether to: (a) update s3-ultra to actually call `next_slice` and benchmark
//!   the result, or (b) leave the static pool as-is and simply publish this module as a
//!   library convenience for future callers.
//! - See also: `s3-ultra/docs/Architecture-Guide.md` "GET body generation" section and
//!   the TODO block in `s3-ultra/src/s3_backend.rs`.
//!
//! # Usage in an async HTTP server
//!
//! ```rust
//! use bytes::Bytes;
//! use dgen_data::thread_local::{next_slice, reconfigure};
//!
//! // Serve a GET response body as a stream of chunks.
//! // Call this function inside a stream::unfold closure (before any .await).
//! fn get_chunk(chunk_size: usize) -> Bytes {
//!     next_slice(chunk_size)  // zero-copy Arc slice for size ≤ 1 MiB
//! }
//! ```
//!
//! # Usage in a synchronous benchmark loop
//!
//! ```rust
//! use dgen_data::thread_local::next_slice;
//!
//! let mut total = 0usize;
//! for _ in 0..100_000 {
//!     let buf = next_slice(64 * 1024);  // 64 KB per object
//!     total += buf.len();
//! }
//! assert_eq!(total, 100_000 * 64 * 1024);
//! ```

use std::cell::RefCell;

use bytes::Bytes;

use crate::rolling_pool::RollingPool;

// ── Internal thread-local storage ────────────────────────────────────────────

thread_local! {
    /// Per-thread rolling data pool.
    ///
    /// Created on first use with `dedup=1, compress=1` (incompressible,
    /// non-deduplicated).  Lives until the thread exits.
    static POOL: RefCell<RollingPool> = RefCell::new(RollingPool::new(1, 1));
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Take the next `size` bytes from the calling thread's rolling pool.
///
/// For `size ≤ BLOCK_SIZE` (1 MiB) this is a **zero-copy** operation: the return
/// value is a [`Bytes`] window backed by the same 1 MiB allocation, returned via
/// an Arc reference-count increment only — no `memcpy`, no heap allocation.
///
/// For `size > BLOCK_SIZE` the pool is bypassed and a fresh buffer is generated
/// (Rayon parallel Xoshiro256++).
///
/// # Continuity guarantee
///
/// The internal rolling pointer is **never reset** between calls.  Each call
/// advances the pointer and returns the next slice.  When the 1 MiB backing
/// buffer is exhausted, [`RollingPool`] generates a fresh one with new entropy
/// (`time + urandom`).  This means:
/// - Consecutive calls return distinct bytes (no repeated content).
/// - Different threads each have an independent pool seeded independently.
/// - Repeated GETs of the "same" object receive different bytes — which is the
///   correct behaviour for a fake S3 target that prioritises throughput testing
///   over content reproducibility.
///
/// # Panics
///
/// Panics if `size == 0`.
///
/// # Example
///
/// ```rust
/// use dgen_data::thread_local::next_slice;
///
/// let buf = next_slice(64 * 1024);   // 64 KB, zero-copy
/// assert_eq!(buf.len(), 64 * 1024);
/// ```
pub fn next_slice(size: usize) -> Bytes {
    POOL.with(|p| p.borrow_mut().next_slice(size))
}

/// Change the deduplication and compression factors for the calling thread's pool.
///
/// If either parameter differs from the current value, the backing buffer is
/// immediately discarded and refilled with new data using the new parameters.
/// If both are unchanged, this is a no-op (no refill, no allocation).
///
/// **Thread-local**: only affects the pool on the calling thread.  Call
/// `reconfigure` on every thread (or at startup before the first request) if
/// you want consistent data characteristics across the whole server.
///
/// # Parameters
/// - `dedup`: Deduplication factor.  `1` = no deduplication; `N` = N:1 ratio.
/// - `compress`: Compression factor.  `1` = incompressible; `N` = N:1 ratio.
///
/// # Example
///
/// ```rust
/// use dgen_data::thread_local::{next_slice, reconfigure};
///
/// // Switch to 2:1 compressible data
/// reconfigure(1, 2);
/// let buf = next_slice(4096);
/// assert_eq!(buf.len(), 4096);
/// ```
pub fn reconfigure(dedup: usize, compress: usize) {
    POOL.with(|p| p.borrow_mut().reconfigure(dedup, compress));
}

/// Return the number of bytes remaining in the calling thread's current pool
/// buffer before the next refill.
///
/// Primarily useful for testing and diagnostics.
pub fn remaining() -> usize {
    POOL.with(|p| p.borrow().remaining())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::BLOCK_SIZE;

    #[test]
    fn test_next_slice_returns_correct_size() {
        for size in [1, 64, 512, 4096, 64 * 1024, 256 * 1024, BLOCK_SIZE] {
            let b = next_slice(size);
            assert_eq!(b.len(), size, "expected {size} bytes, got {}", b.len());
        }
    }

    #[test]
    fn test_data_is_not_all_zeros() {
        let b = next_slice(4096);
        assert_ne!(
            &b[..],
            &vec![0u8; 4096][..],
            "pool must not return zeroed data"
        );
    }

    #[test]
    fn test_consecutive_slices_differ() {
        // Rolling pointer advances — consecutive same-size calls must return
        // different byte ranges (within the same backing block).
        let b1 = next_slice(64);
        let b2 = next_slice(64);
        assert_ne!(
            &b1[..],
            &b2[..],
            "consecutive slices must contain different bytes"
        );
    }

    #[test]
    fn test_large_object_handled() {
        // Objects > BLOCK_SIZE bypass the pool but must succeed.
        let size = BLOCK_SIZE + 1;
        let b = next_slice(size);
        assert_eq!(b.len(), size);
    }

    #[test]
    fn test_many_slices_across_refill_boundary() {
        // Drive the pool through multiple refills without error.
        let chunk = 256 * 1024; // 256 KiB — typical HTTP chunk size
        let total = 8 * BLOCK_SIZE; // 8 MiB — forces 8 refills
        let iterations = total / chunk;
        for i in 0..iterations {
            let b = next_slice(chunk);
            assert_eq!(b.len(), chunk, "iteration {i}: wrong size");
        }
    }

    #[test]
    fn test_reconfigure_succeeds() {
        // Change parameters — must not panic or corrupt the pool.
        reconfigure(1, 2); // 2:1 compressible
        let b = next_slice(4096);
        assert_eq!(b.len(), 4096);
        reconfigure(1, 1); // reset to incompressible
    }

    #[test]
    fn test_remaining_decreases_then_resets() {
        // Consume bytes until a refill happens; remaining should jump back up.
        let chunk = 64 * 1024;
        // Fill until we see a refill: remaining() must have jumped from < chunk to > chunk.
        let mut saw_reset = false;
        let mut prev = remaining();
        for _ in 0..(BLOCK_SIZE / chunk + 2) {
            next_slice(chunk);
            let now = remaining();
            if now > prev {
                saw_reset = true;
                break;
            }
            prev = now;
        }
        assert!(saw_reset, "expected remaining() to reset after pool refill");
    }

    #[test]
    fn test_total_bytes_served_is_accurate() {
        let chunk = 65_537; // awkward size that doesn't divide BLOCK_SIZE
        let count = 100;
        let mut total = 0usize;
        for _ in 0..count {
            let b = next_slice(chunk);
            total += b.len();
        }
        assert_eq!(total, chunk * count);
    }

    /// Verify that a future wrapping next_slice is Send (compile-time invariant).
    ///
    /// The key: `POOL.with(...)` borrow is acquired and released in one
    /// synchronous expression before any await point, so the future holds no
    /// reference to the thread-local `POOL` across yields.
    #[test]
    fn test_next_slice_result_is_send() {
        // Bytes is Send; calling next_slice returns Bytes.
        // The future wrapping the call must therefore be Send too.
        fn assert_send<T: Send>(_: T) {}

        // This future calls next_slice synchronously then holds the result across
        // a pending Future::poll boundary (std::future::pending is a !Unpin future
        // that never resolves, so the compiler must prove Send across the yield).
        async fn send_across_yield(size: usize) -> Bytes {
            let b = next_slice(size); // borrow released here; no thread-local ref held
            std::future::pending::<()>().await;
            b
        }

        assert_send(send_across_yield(4096));
    }
}
