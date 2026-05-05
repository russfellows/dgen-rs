// src/xor_stream.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fast, dedup-safe data generation via XOR keystream.
//!
//! # Design  (matches MinIO warp `rngfix`)
//!
//! [`UniqueXorStream`] maintains a **16 KiB source buffer** that is filled once
//! at construction with high-entropy random data and then never modified.
//! 16 KiB fits entirely in L1 cache (typical L1 = 32 KiB per core), so
//! source reads are effectively free.
//!
//! The logical output is an infinite pseudorandom byte stream.  Each [`fill`]
//! call atomically claims the next `buf.len()` bytes from that stream.
//!
//! ## Key derivation  (per 16 KiB block)
//!
//! The stream is divided into **16 KiB blocks**.  For block `n`:
//!
//! ```text
//! base_mix = xxh3_scalar(n) XOR (n × 11400714785074694791)
//! keys[i]  = base_mix XOR subxor[i]          i = 0..3
//! ```
//!
//! `subxor[0..3]` are four secret 64-bit values generated at construction.
//! This is a direct port of warp's key schedule (`scrambleU64` + `subxor` mix).
//!
//! ## Inner loop  (32 bytes per iteration)
//!
//! Within a block, output bytes are generated as:
//! ```text
//! out[p] = base[p] XOR keys[(p / 8) % 4]
//! ```
//! The 32-byte key cycle aligns with AVX2 registers.  With the source buffer
//! L1-hot and the output going to DRAM, the bottleneck is **DRAM write
//! bandwidth** — identical to warp's behaviour.
//!
//! ## Why this is fast
//!
//! | Factor         | Old design (Xoshiro)    | New design (warp-style)          |
//! |----------------|-------------------------|----------------------------------|
//! | Ops per byte   | ~0.75 (Xoshiro per 8 B) | ~0.00006 (12 ops per 16 KiB)     |
//! | Source reads   | 1 MiB (spills L2)       | 16 KiB (stays in L1)             |
//! | Expected GB/s  | ~2 GB/s single core     | ~8–15 GB/s single core           |
//!
//! ## Dedup safety
//!
//! The byte-offset counter advances monotonically, so no two `fill()` calls
//! ever produce the same range from the stream.  Content-fingerprint dedup
//! (SHA-256 / MD5 of 512 B or larger blocks) sees zero collisions between
//! any two calls.
//!
//! ## Thread safety
//!
//! `fill()` takes `&self`.  The byte offset is claimed via `AtomicU64`.
//! Key derivation is purely deterministic — no shared mutable state.

use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Log₂ of the source buffer size (14 → 16 KiB).
const BUFFER_LOG: u32 = 14;

/// Source buffer size: 16 KiB.  Fits in L1 cache.
pub const XOR_BASE_SIZE: usize = 1 << BUFFER_LOG;

/// Bitwise mask to wrap a byte offset into the source buffer.
const BUFFER_MASK: usize = XOR_BASE_SIZE - 1;

/// Large prime used in warp's block-key mixing.
const BLOCK_PRIME: u64 = 11_400_714_785_074_694_791;

// ── Public struct ─────────────────────────────────────────────────────────────

/// Fast, dedup-safe, low-CPU data generator.
///
/// Intended as a **process-level singleton** shared across threads.
/// Thread-safe: the base buffer and subxor keys are immutable after
/// construction; state is limited to two `AtomicU64` counters.
///
/// # Example
///
/// ```rust
/// use dgen_data::UniqueXorStream;
///
/// let stream = UniqueXorStream::new();
/// let mut buf = vec![0u8; 8 * 1024 * 1024];
/// stream.fill(&mut buf);   // first object
/// stream.fill(&mut buf);   // second object — guaranteed different bytes
/// ```
pub struct UniqueXorStream {
    /// 16 KiB of immutable random data.  Always hot in L1 cache.
    base: Box<[u8; XOR_BASE_SIZE]>,
    /// Four secret 64-bit values generated at construction, mixed into every
    /// block's key schedule.  Makes the stream unique per process start.
    subxor: [u64; 4],
    /// Monotonically increasing byte offset into the logical infinite stream.
    /// Each `fill()` call advances this by `buf.len()`.  Never reset.
    offset: AtomicU64,
    /// Number of `fill()` calls made so far.  Used by `objects_generated()`.
    call_count: AtomicU64,
}

// SAFETY: `base` and `subxor` are never written after construction.
// `offset` and `call_count` are `AtomicU64` which are inherently `Sync + Send`.
unsafe impl Sync for UniqueXorStream {}
unsafe impl Send for UniqueXorStream {}

impl UniqueXorStream {
    /// Create a new `UniqueXorStream`.
    ///
    /// Fills the 16 KiB base buffer and generates 4 secret `subxor` keys
    /// using a Xoshiro256++ RNG seeded from the system clock and OS entropy.
    pub fn new() -> Self {
        let seed = entropy_seed();
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);

        // Initialise the 16 KiB base buffer.
        let mut base = Box::new([0u8; XOR_BASE_SIZE]);
        rng.fill_bytes(base.as_mut_slice());

        // Generate 4 secret mixing keys.
        let subxor = [
            rng.next_u64(),
            rng.next_u64(),
            rng.next_u64(),
            rng.next_u64(),
        ];

        Self {
            base,
            subxor,
            offset: AtomicU64::new(0),
            call_count: AtomicU64::new(0),
        }
    }

    /// Fill `buf` with unique, dedup-safe pseudorandom bytes.
    ///
    /// Atomically claims the next `buf.len()` bytes of the infinite stream.
    /// No reseeding, no per-call allocations.  Thread-safe.
    pub fn fill(&self, buf: &mut [u8]) {
        let start = self.offset.fetch_add(buf.len() as u64, Ordering::Relaxed);
        self.call_count.fetch_add(1, Ordering::Relaxed);
        xor_fill(&self.base, &self.subxor, start, buf);
    }

    /// Number of `fill()` calls made so far.
    ///
    /// Equivalent to the number of "objects" generated when each `fill()` call
    /// corresponds to one object.  Thread-safe diagnostic counter.
    pub fn objects_generated(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }

    /// Total bytes generated so far (sum of all `buf.len()` arguments to `fill()`).
    pub fn bytes_generated(&self) -> u64 {
        self.offset.load(Ordering::Relaxed)
    }
}

impl Default for UniqueXorStream {
    fn default() -> Self {
        Self::new()
    }
}

// ── Core fill logic ───────────────────────────────────────────────────────────

/// Fill `out` starting at `stream_offset` in the logical stream.
///
/// Processes the output in at-most-16-KiB blocks, deriving 4 keys per block
/// and applying them via `xor_chunk`.  This is the warp `rngfix` algorithm.
#[inline]
pub(crate) fn xor_fill(
    base: &[u8; XOR_BASE_SIZE],
    subxor: &[u64; 4],
    stream_offset: u64,
    out: &mut [u8],
) {
    let mut remaining = out;
    let mut offset = stream_offset;

    while !remaining.is_empty() {
        let within_block = (offset as usize) & BUFFER_MASK;
        let bytes_this_block = (XOR_BASE_SIZE - within_block).min(remaining.len());

        let (chunk, rest) = remaining.split_at_mut(bytes_this_block);

        // Derive 4 keys for this 16 KiB block — computed only once per block.
        let block_n = offset >> BUFFER_LOG;
        let keys = derive_keys(block_n, subxor);

        xor_chunk(base, within_block, chunk, &keys);

        offset += bytes_this_block as u64;
        remaining = rest;
    }
}

/// Derive 4 XOR keys for block `block_n`.
///
/// Direct port of warp's `Read()` key schedule:
/// `keys[i] = scrambleU64(blockN) ^ subxor[i] ^ (blockN × BLOCK_PRIME)`
#[inline(always)]
fn derive_keys(block_n: u64, subxor: &[u64; 4]) -> [u64; 4] {
    let mix = scramble_u64(block_n) ^ block_n.wrapping_mul(BLOCK_PRIME);
    [
        mix ^ subxor[0],
        mix ^ subxor[1],
        mix ^ subxor[2],
        mix ^ subxor[3],
    ]
}

/// XOR `base[base_start..]` into `out` using a 32-byte key cycle.
///
/// Precondition: `base_start + out.len() <= XOR_BASE_SIZE`.
///
/// The key for base position `p` is `keys[(p / 8) % 4]` — the same mapping
/// as warp's `xorSlice` SSE2 assembly: a 32-byte key pattern repeating over
/// the 16 KiB base buffer.
///
/// Implementation strategy for autovectorisation:
/// 1. Take `src = &base[base_start..base_start+n]` — a sub-slice whose length
///    LLVM knows is `n`.  Both `src[i+j]` and `out[i+j]` are then provably
///    in-bounds when `i+32 <= n` and `j < 32`, so all bounds checks are
///    elided in the hot loop.
/// 2. Byte-by-byte head until the base position is 32-byte aligned.
/// 3. Full 32-byte chunks — fixed inner loop over j=0..32 with
///    `out[i+j] = src[i+j] ^ cycle[j]`.  LLVM widens this to VPXOR ymm
///    (AVX2) or PXOR xmm (SSE2) automatically.
/// 4. Byte-by-byte tail.
#[inline]
fn xor_chunk(
    base: &[u8; XOR_BASE_SIZE],
    base_start: usize,
    out: &mut [u8],
    keys: &[u64; 4],
) {
    debug_assert!(base_start + out.len() <= XOR_BASE_SIZE);

    let n = out.len();
    // Sub-slice: LLVM now knows src.len() == n.
    // Accesses src[i+j] with i+32<=n and j<32 are provably in-bounds → no checks.
    let src = &base[base_start..base_start + n];

    // Expand 4 keys → 32-byte key cycle.
    let cycle: [u8; 32] = {
        let mut c = [0u8; 32];
        c[0..8].copy_from_slice(&keys[0].to_le_bytes());
        c[8..16].copy_from_slice(&keys[1].to_le_bytes());
        c[16..24].copy_from_slice(&keys[2].to_le_bytes());
        c[24..32].copy_from_slice(&keys[3].to_le_bytes());
        c
    };

    let mut i = 0usize;

    // ── Head: bytes until `base_start + i` reaches the next 32-byte boundary ─
    let head = ((32 - (base_start & 31)) & 31).min(n);
    while i < head {
        out[i] = src[i] ^ cycle[(base_start + i) & 31];
        i += 1;
    }

    // ── Hot path: full 32-byte chunks ────────────────────────────────────────
    // After the head, (base_start + i) % 32 == 0, so cycle[j] is the correct
    // key byte for base position base_start+i+j.  All three arrays have
    // bound == n (out, src) or 32 (cycle), and LLVM can prove every access is
    // in-bounds from the loop conditions → autovectorises to PXOR/VPXOR.
    while i + 32 <= n {
        for j in 0..32 {
            out[i + j] = src[i + j] ^ cycle[j];
        }
        i += 32;
    }

    // ── Tail: remaining 0–31 bytes ────────────────────────────────────────────
    while i < n {
        out[i] = src[i] ^ cycle[(base_start + i) & 31];
        i += 1;
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// xxh3 scalar finaliser for a single 64-bit value.
///
/// Direct port of warp's `scrambleU64()`.  Maps a sequential block number to
/// a well-distributed 64-bit value in ~12 arithmetic operations.
#[inline(always)]
fn scramble_u64(v: u64) -> u64 {
    let mut h = v ^ (0x1cad21f72c81017c_u64 ^ 0xdb979083e96dd4de_u64);
    h = h.rotate_left(49) ^ h.rotate_left(24);
    h = h.wrapping_mul(0x9fb21c651e98df25);
    h ^= (h >> 35).wrapping_add(8);
    h = h.wrapping_mul(0x9fb21c651e98df25);
    h ^ (h >> 28)
}

/// Seed from wall-clock nanoseconds XOR'd with OS entropy via `rand::rng()`.
fn entropy_seed() -> u64 {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut rng = rand::rng();
    time.wrapping_add(rng.next_u64())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

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
        // Two independent instances each get different base buffers and subxor keys.
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
        // No 512-byte block in object A should match any block in object B.
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
        assert!(
            ratio < 0.02,
            "zero byte ratio {:.2}% too high — data may not be random",
            ratio * 100.0
        );
    }

    #[test]
    fn objects_generated_counts_calls() {
        let stream = UniqueXorStream::new();
        assert_eq!(stream.objects_generated(), 0);
        let mut buf = vec![0u8; 1024];
        stream.fill(&mut buf);
        assert_eq!(stream.objects_generated(), 1);
        stream.fill(&mut buf);
        assert_eq!(stream.objects_generated(), 2);
    }

    #[test]
    fn xor_chunk_correctness() {
        // Verify that xor_chunk produces the expected XOR of base and key.
        let mut base = Box::new([0u8; XOR_BASE_SIZE]);
        for (i, b) in base.iter_mut().enumerate() {
            *b = (i & 0xff) as u8;
        }
        let keys = [0x0102030405060708u64, 0u64, 0u64, 0u64];
        let mut out = vec![0u8; 8];
        xor_chunk(&base, 0, &mut out, &keys);
        // base[0..8] ^ keys[0].to_le_bytes()
        let expected: Vec<u8> = (0u8..8)
            .zip(0x0102030405060708u64.to_le_bytes().iter())
            .map(|(b, &k)| b ^ k)
            .collect();
        assert_eq!(out, expected, "xor_chunk byte 0..8 mismatch");
    }

    #[test]
    fn xor_fill_cross_block_boundary() {
        // Generate data that spans multiple 16 KiB blocks and verify
        // that the result is different from a single-block fill at the same offset.
        let base = Box::new([0xABu8; XOR_BASE_SIZE]);
        let subxor = [1u64, 2, 3, 4];

        let size = XOR_BASE_SIZE * 3 + 1000; // spans 4 blocks
        let mut out1 = vec![0u8; size];
        let mut out2 = vec![0u8; size];

        xor_fill(&base, &subxor, 0, &mut out1);
        xor_fill(&base, &subxor, 0, &mut out2);

        assert_eq!(out1, out2, "deterministic fill must produce identical results");

        // Also verify the second 16 KiB block differs from the first.
        assert_ne!(
            &out1[..XOR_BASE_SIZE],
            &out1[XOR_BASE_SIZE..XOR_BASE_SIZE * 2],
            "adjacent blocks must differ (different keys)"
        );
    }
}
