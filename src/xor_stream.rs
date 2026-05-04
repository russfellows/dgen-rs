// src/xor_stream.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fast, dedup-safe data generation via XOR keystream.
//!
//! # Design
//!
//! [`UniqueXorStream`] holds a 1 MiB base buffer filled once with high-entropy
//! random data.  For each [`fill()`](UniqueXorStream::fill) call it:
//!
//! 1. Atomically increments a global counter to obtain a unique `object_id`.
//! 2. Derives a per-call Xoshiro256++ seed from `object_id` via splitmix64
//!    (one pass of a high-avalanche hash so that sequential IDs produce
//!    completely different RNG states).
//! 3. Generates a keystream from that Xoshiro256++ instance.
//! 4. XORs the keystream with the cycling 1 MiB base buffer into the output.
//!
//! # Dedup safety
//!
//! - **Inter-object**: every call gets a unique `object_id` → unique seed →
//!   unique keystream → every output byte differs.
//! - **Intra-object**: Xoshiro256++ advances its state between every 8-byte
//!   word, so consecutive 512-byte blocks within the same object also differ.
//! - **Fingerprint safety**: any content-fingerprint deduplication scheme
//!   (SHA-256/MD5 of 512 B / 4 KiB blocks) will see zero matches across any
//!   two `fill()` calls.  Delta-dedup systems that compare raw byte streams
//!   *could* detect the XOR relationship in theory, but no production storage
//!   product does block-level delta analysis.
//!
//! # Performance
//!
//! Xoshiro256++ generates ~15–20 GB/s per core.  For an 8 MiB object that is
//! roughly 0.4–0.5 ms — with no allocations, no Rayon, and no thread
//! synchronisation beyond a single `AtomicU64::fetch_add`.

use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 1 MiB base buffer size.  Fits in L2 cache on CPUs with ≥1 MiB per-core L2.
/// Divisible by 8 (2^20 / 2^3 = 2^17), so the 8-byte word loop never straddles
/// the end of the base buffer.
pub const XOR_BASE_SIZE: usize = 1024 * 1024;

/// Fast, dedup-safe data generator.
///
/// Intended as a **process-level singleton** shared across all threads.
/// Thread-safe: the base buffer is immutable after construction; the counter
/// uses `AtomicU64` with `Relaxed` ordering (uniqueness, not causality, is
/// required).
///
/// # Example
///
/// ```rust
/// use dgen_data::UniqueXorStream;
///
/// let stream = UniqueXorStream::new();
/// let mut buf = vec![0u8; 8 * 1024 * 1024];
/// stream.fill(&mut buf);   // first object
/// stream.fill(&mut buf);   // second object — different bytes, guaranteed
/// ```
pub struct UniqueXorStream {
    /// 1 MiB of truly random data, read-only after construction.
    base: Box<[u8]>,
    /// Global object counter.  Monotonically increasing; each `fill()` call
    /// consumes one ID.  Wrapping on overflow is fine — 2^64 objects is
    /// effectively unreachable in practice.
    counter: AtomicU64,
}

// SAFETY: `base` is never written after construction (shared read-only).
// `counter` is `AtomicU64` which is inherently `Sync + Send`.
unsafe impl Sync for UniqueXorStream {}
unsafe impl Send for UniqueXorStream {}

impl UniqueXorStream {
    /// Create a new `UniqueXorStream`.
    ///
    /// Fills the 1 MiB base buffer with Xoshiro256++ output seeded from the
    /// system clock plus `getrandom` entropy, so each process start (and each
    /// explicit construction) gets a distinct base.
    pub fn new() -> Self {
        let seed = entropy_seed();
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
        let mut base = vec![0u8; XOR_BASE_SIZE];
        rng.fill_bytes(&mut base);
        Self {
            base: base.into_boxed_slice(),
            counter: AtomicU64::new(0),
        }
    }

    /// Fill `buf` with unique, dedup-safe data.
    ///
    /// Thread-safe and lock-free.  Each call is guaranteed to produce output
    /// that does not share any 512-byte (or larger) block fingerprint with any
    /// other call, regardless of which thread calls it.
    ///
    /// `buf` may be any size.  Objects larger than the 1 MiB base have the base
    /// tiled (cycled) through the output; uniqueness still holds because the
    /// XOR keystream advances independently of the base cycle.
    pub fn fill(&self, buf: &mut [u8]) {
        let object_id = self.counter.fetch_add(1, Ordering::Relaxed);
        // splitmix64: maps sequential IDs to uncorrelated 64-bit values.
        // This ensures that IDs 0, 1, 2, … produce completely different
        // Xoshiro256++ states (without it, consecutive seeds produce similar
        // initial outputs).
        let seed = splitmix64(object_id);
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);

        let base = &*self.base;
        // XOR_BASE_SIZE is 2^20; i*8 is always a multiple of 8; so
        // (i*8) % XOR_BASE_SIZE is always a multiple of 8 and never within 7
        // bytes of the end.  The slice [base_off..base_off+8] is always valid.
        let base_len = base.len(); // == XOR_BASE_SIZE

        let chunks = buf.len() / 8;
        for i in 0..chunks {
            let key = rng.next_u64();
            let base_off = (i * 8) % base_len;
            // SAFETY: base_off is a multiple of 8, base_len is a multiple of 8,
            // so base_off + 8 <= base_len always holds.
            let base_word =
                u64::from_le_bytes(base[base_off..base_off + 8].try_into().unwrap());
            let out = key ^ base_word;
            buf[i * 8..i * 8 + 8].copy_from_slice(&out.to_le_bytes());
        }

        // Handle trailing 1–7 bytes (only occurs for non-8-byte-aligned sizes).
        let rem_start = chunks * 8;
        if rem_start < buf.len() {
            let key = rng.next_u64();
            let key_bytes = key.to_le_bytes();
            let base_off = rem_start % base_len;
            for j in 0..(buf.len() - rem_start) {
                buf[rem_start + j] = base[base_off + j] ^ key_bytes[j];
            }
        }
    }

    /// Return the number of objects generated so far.
    ///
    /// Intended for diagnostics / logging only.
    pub fn objects_generated(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }
}

impl Default for UniqueXorStream {
    fn default() -> Self {
        Self::new()
    }
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// splitmix64 finalizer — converts a sequential counter into a well-distributed
/// 64-bit value suitable for use as a Xoshiro seed.
///
/// Reference: https://prng.di.unimi.it/splitmix64.c
#[inline(always)]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Generate a seed from wall-clock time XOR'd with OS entropy.
fn entropy_seed() -> u64 {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut rng = rand::rng();
    time.wrapping_add(rng.next_u64())
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_objects_produce_different_bytes() {
        let stream = UniqueXorStream::new();
        let mut a = vec![0u8; 8 * 1024 * 1024];
        let mut b = vec![0u8; 8 * 1024 * 1024];
        stream.fill(&mut a);
        stream.fill(&mut b);
        assert_ne!(a, b, "consecutive fills must produce different bytes");
    }

    #[test]
    fn two_streams_produce_different_bytes() {
        // Two independent instances each get different base buffers.
        let s1 = UniqueXorStream::new();
        let s2 = UniqueXorStream::new();
        let mut a = vec![0u8; 64 * 1024];
        let mut b = vec![0u8; 64 * 1024];
        s1.fill(&mut a);
        s2.fill(&mut b);
        assert_ne!(a, b, "independent streams must produce different bytes");
    }

    #[test]
    fn no_512b_block_collision() {
        // Verify that no 512-byte block in object A matches any block in object B.
        let stream = UniqueXorStream::new();
        let mut a = vec![0u8; 4 * 1024 * 1024];
        let mut b = vec![0u8; 4 * 1024 * 1024];
        stream.fill(&mut a);
        stream.fill(&mut b);

        let block_size = 512;
        let blocks_a: std::collections::HashSet<&[u8]> = a.chunks(block_size).collect();
        let collisions = b
            .chunks(block_size)
            .filter(|blk| blocks_a.contains(blk))
            .count();
        assert_eq!(collisions, 0, "no 512-byte block should collide across objects");
    }

    #[test]
    fn fill_is_thread_safe() {
        use std::sync::Arc;
        let stream = Arc::new(UniqueXorStream::new());
        let mut handles = Vec::new();
        for _ in 0..16 {
            let s = Arc::clone(&stream);
            handles.push(std::thread::spawn(move || {
                let mut buf = vec![0u8; 1024 * 1024];
                s.fill(&mut buf);
                buf
            }));
        }
        let results: Vec<Vec<u8>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        // All 16 results must be unique.
        for i in 0..results.len() {
            for j in (i + 1)..results.len() {
                assert_ne!(results[i], results[j], "threads {i} and {j} produced identical data");
            }
        }
    }

    #[test]
    fn low_zero_byte_ratio() {
        let stream = UniqueXorStream::new();
        let mut buf = vec![0u8; 8 * 1024 * 1024];
        stream.fill(&mut buf);
        let zeros = buf.iter().filter(|&&b| b == 0).count();
        let ratio = zeros as f64 / buf.len() as f64;
        // Random data should have ~0.39% zeros; allow up to 2% for statistical slack.
        assert!(ratio < 0.02, "zero byte ratio {:.2}% too high — data may not be random", ratio * 100.0);
    }
}
