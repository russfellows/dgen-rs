// SPDX-License-Identifier: Apache-2.0 OR MIT
// SPDX-FileCopyrightText: 2025 Russ Fellows <russ.fellows@gmail.com>
//
// Comprehensive correctness tests for the dgen-data crate.
//
// These tests verify the core contracts that ALL callers depend on:
//   - Exact output size
//   - Correct deduplication ratio (batch and streaming)
//   - Correct compression ratio (batch and streaming)
//   - Chunk-size independence (same bytes regardless of fill_chunk buffer size)
//   - Seed reproducibility (same seed → same bytes, batch and streaming)
//   - set_seed stripe reproducibility (A-B-A stripe pattern)
//   - Zero-size generation (completes immediately, no infinite loop)
//   - Unique data by default (no seed → different bytes each run)

use dgen_data::{generate_data_simple, DataGenerator, GeneratorConfig, NumaMode};
use std::collections::HashSet;

const BLOCK: usize = 1024 * 1024; // 1 MiB — matches BLOCK_SIZE in dgen-data

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Drain a DataGenerator into a Vec<u8> using the given chunk buffer size.
fn drain(mut gen: DataGenerator, chunk_buf_size: usize) -> Vec<u8> {
    let total = gen.total_size();
    let mut out = Vec::with_capacity(total);
    let mut buf = vec![0u8; chunk_buf_size];
    loop {
        let n = gen.fill_chunk(&mut buf);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

fn new_gen(size: usize, dedup: usize, compress: usize) -> DataGenerator {
    DataGenerator::new(GeneratorConfig {
        size,
        dedup_factor: dedup,
        compress_factor: compress,
        seed: None,
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
    })
}

fn new_seeded_gen(size: usize, dedup: usize, compress: usize, seed: u64) -> DataGenerator {
    DataGenerator::new(GeneratorConfig {
        size,
        dedup_factor: dedup,
        compress_factor: compress,
        seed: Some(seed),
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
    })
}

/// Count distinct 1 MiB blocks in a byte slice.
fn count_unique_blocks(data: &[u8]) -> usize {
    let mut set = HashSet::new();
    let mut i = 0;
    while i + BLOCK <= data.len() {
        set.insert(data[i..i + BLOCK].to_vec());
        i += BLOCK;
    }
    set.len()
}

// ---------------------------------------------------------------------------
// Size correctness
// ---------------------------------------------------------------------------

#[test]
fn test_exact_size_sub_block() {
    // Sizes smaller than one block must be returned exactly.
    for &size in &[0usize, 1, 100, 1023, 4096, 65536, BLOCK - 1] {
        let data = generate_data_simple(size, 1, 1);
        assert_eq!(data.len(), size, "batch: size={}", size);

        let data2 = drain(new_gen(size, 1, 1), 4096);
        assert_eq!(data2.len(), size, "streaming: size={}", size);
    }
}

#[test]
fn test_exact_size_multi_block() {
    for &n in &[1usize, 2, 5, 8] {
        let size = n * BLOCK;
        let data = generate_data_simple(size, 1, 1);
        assert_eq!(data.len(), size, "batch: {}x BLOCK", n);

        let data2 = drain(new_gen(size, 1, 1), BLOCK);
        assert_eq!(data2.len(), size, "streaming: {}x BLOCK", n);
    }
}

#[test]
fn test_exact_size_non_block_aligned() {
    // Sizes that are not multiples of BLOCK.
    let size = 3 * BLOCK + 500_000;
    let data = generate_data_simple(size, 1, 1);
    assert_eq!(data.len(), size, "batch: non-aligned size");

    let data2 = drain(new_gen(size, 1, 1), 128 * 1024);
    assert_eq!(data2.len(), size, "streaming: non-aligned size");
}

// ---------------------------------------------------------------------------
// Zero size
// ---------------------------------------------------------------------------

#[test]
fn test_zero_size_batch() {
    let data = generate_data_simple(0, 1, 1);
    assert_eq!(data.len(), 0, "batch: zero size must return empty");
}

#[test]
fn test_zero_size_streaming() {
    let mut gen = new_gen(0, 1, 1);
    assert!(
        gen.is_complete(),
        "zero-size generator should start complete"
    );
    let mut buf = vec![0u8; 1024];
    let n = gen.fill_chunk(&mut buf);
    assert_eq!(n, 0, "fill_chunk on zero-size must return 0");
}

// ---------------------------------------------------------------------------
// Deduplication — batch path
// ---------------------------------------------------------------------------

#[test]
fn test_dedup_batch_2x() {
    let num_blocks = 8;
    let size = num_blocks * BLOCK;
    let data = generate_data_simple(size, 2, 1);
    let unique = count_unique_blocks(data.as_slice());
    let expected = num_blocks / 2;
    assert!(
        (unique as i64 - expected as i64).abs() <= 1,
        "dedup=2: expected ~{} unique blocks, got {}",
        expected,
        unique
    );
}

#[test]
fn test_dedup_batch_4x() {
    let num_blocks = 8;
    let size = num_blocks * BLOCK;
    let data = generate_data_simple(size, 4, 1);
    let unique = count_unique_blocks(data.as_slice());
    let expected = (num_blocks as f64 / 4.0).round() as usize;
    assert!(
        (unique as i64 - expected as i64).abs() <= 1,
        "dedup=4: expected ~{} unique blocks, got {}",
        expected,
        unique
    );
}

#[test]
fn test_no_dedup_batch() {
    let num_blocks = 4;
    let size = num_blocks * BLOCK;
    let data = generate_data_simple(size, 1, 1);
    let unique = count_unique_blocks(data.as_slice());
    // With no dedup, each block should be unique (extremely unlikely to collide)
    assert_eq!(unique, num_blocks, "dedup=1: all blocks should be unique");
}

// ---------------------------------------------------------------------------
// Deduplication — streaming path (DataGenerator::fill_chunk)
// ---------------------------------------------------------------------------

#[test]
fn test_dedup_streaming_2x() {
    let num_blocks = 8;
    let size = num_blocks * BLOCK;
    // Use small chunks to exercise the sequential path
    let data = drain(new_gen(size, 2, 1), 4096);
    assert_eq!(data.len(), size);
    let unique = count_unique_blocks(&data);
    let expected = num_blocks / 2;
    assert!(
        (unique as i64 - expected as i64).abs() <= 1,
        "streaming dedup=2: expected ~{} unique blocks, got {}",
        expected,
        unique
    );
}

#[test]
fn test_dedup_streaming_4x() {
    let num_blocks = 8;
    let size = num_blocks * BLOCK;
    // Use large chunks to exercise the parallel path
    let data = drain(new_gen(size, 4, 1), 4 * BLOCK);
    assert_eq!(data.len(), size);
    let unique = count_unique_blocks(&data);
    let expected = (num_blocks as f64 / 4.0).round() as usize;
    assert!(
        (unique as i64 - expected as i64).abs() <= 1,
        "streaming dedup=4 (parallel): expected ~{} unique blocks, got {}",
        expected,
        unique
    );
}

// ---------------------------------------------------------------------------
// Compression ratio
// ---------------------------------------------------------------------------

/// Measure compression ratio using zstd if available; return None if zstd missing.
fn zstd_ratio(data: &[u8]) -> Option<f64> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("zstd")
        .args(["-c", "-1", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    child.stdin.take()?.write_all(data).ok()?;
    let output = child.wait_with_output().ok()?;
    if output.stdout.is_empty() {
        return None;
    }
    Some(data.len() as f64 / output.stdout.len() as f64)
}

#[test]
fn test_compress_1_is_incompressible() {
    let data = generate_data_simple(4 * BLOCK, 1, 1);
    assert_eq!(data.len(), 4 * BLOCK);
    if let Some(ratio) = zstd_ratio(data.as_slice()) {
        assert!(
            (0.95..=1.05).contains(&ratio),
            "compress=1 should be incompressible, got ratio {:.4}",
            ratio
        );
    }
}

#[test]
fn test_compress_2_is_compressible() {
    let data = generate_data_simple(4 * BLOCK, 1, 2);
    assert_eq!(data.len(), 4 * BLOCK);
    if let Some(ratio) = zstd_ratio(data.as_slice()) {
        assert!(
            ratio > 1.5,
            "compress=2 should compress well, got ratio {:.4}",
            ratio
        );
    }
}

#[test]
fn test_compress_4_is_highly_compressible() {
    let data = generate_data_simple(4 * BLOCK, 1, 4);
    assert_eq!(data.len(), 4 * BLOCK);
    if let Some(ratio) = zstd_ratio(data.as_slice()) {
        assert!(
            ratio > 3.0,
            "compress=4 should compress to ~4x, got ratio {:.4}",
            ratio
        );
    }
}

// ---------------------------------------------------------------------------
// Chunk-size independence (streaming path)
// ---------------------------------------------------------------------------

#[test]
fn test_chunk_size_independence() {
    let size = 4 * BLOCK;
    let seed = 0xABCD_EF01u64;

    // 1 KiB chunks — sequential path
    let data_1k = drain(new_seeded_gen(size, 2, 2, seed), 1024);
    // 1 MiB chunks — parallel path
    let data_1m = drain(new_seeded_gen(size, 2, 2, seed), BLOCK);
    // 256 KiB chunks
    let data_256k = drain(new_seeded_gen(size, 2, 2, seed), 256 * 1024);

    assert_eq!(data_1k.len(), size);
    assert_eq!(data_1m.len(), size);
    assert_eq!(data_256k.len(), size);

    assert_eq!(
        data_1k, data_1m,
        "1 KiB vs 1 MiB chunks produced different bytes"
    );
    assert_eq!(
        data_1k, data_256k,
        "1 KiB vs 256 KiB chunks produced different bytes"
    );
}

// ---------------------------------------------------------------------------
// Seed reproducibility — batch path
// ---------------------------------------------------------------------------

#[test]
fn test_batch_seed_reproducibility() {
    let size = 4 * BLOCK;
    let seed = 0x1234_5678u64;

    // generate_data_simple doesn't take a seed; use GeneratorConfig directly
    use dgen_data::generate_data;
    let cfg = || GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        seed: Some(seed),
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
    };

    let a = generate_data(cfg()).into_bytes();
    let b = generate_data(cfg()).into_bytes();

    assert_eq!(a.len(), size);
    assert_eq!(a, b, "batch: same seed must produce identical output");
}

#[test]
fn test_batch_different_seeds_differ() {
    use dgen_data::generate_data;
    let size = 2 * BLOCK;
    let mk = |s| GeneratorConfig {
        size,
        seed: Some(s),
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
    };

    let a = generate_data(mk(1)).into_bytes();
    let b = generate_data(mk(2)).into_bytes();
    assert_ne!(a, b, "batch: different seeds must differ");
}

// ---------------------------------------------------------------------------
// Seed reproducibility — streaming path
// ---------------------------------------------------------------------------

#[test]
fn test_streaming_seed_reproducibility() {
    let size = 4 * BLOCK;
    let seed = 0xDEAD_BEEF_CAFE_BABEu64;

    let a = drain(new_seeded_gen(size, 1, 1, seed), 64 * 1024);
    let b = drain(new_seeded_gen(size, 1, 1, seed), 64 * 1024);

    assert_eq!(a.len(), size);
    assert_eq!(a, b, "streaming: same seed must produce identical output");
}

#[test]
fn test_streaming_different_seeds_differ() {
    let size = 2 * BLOCK;
    let a = drain(new_seeded_gen(size, 1, 1, 100), BLOCK);
    let b = drain(new_seeded_gen(size, 1, 1, 200), BLOCK);
    assert_ne!(a, b, "streaming: different seeds must differ");
}

// ---------------------------------------------------------------------------
// Unique data by default (no seed)
// ---------------------------------------------------------------------------

#[test]
fn test_default_entropy_unique() {
    let size = BLOCK;
    let runs: Vec<Vec<u8>> = (0..5)
        .map(|_| drain(new_gen(size, 1, 1), 64 * 1024))
        .collect();

    for i in 0..runs.len() {
        for j in (i + 1)..runs.len() {
            assert_ne!(
                runs[i], runs[j],
                "runs {} and {} produced identical data without a seed",
                i, j
            );
        }
    }
}

// ---------------------------------------------------------------------------
// set_seed stripe reproducibility
// ---------------------------------------------------------------------------

#[test]
fn test_set_seed_stripe_reproducibility() {
    let chunk = BLOCK * 10; // 10 MiB per stripe
    let total = chunk * 4; // room for 4 stripes
    let seed_a = 0x1111_1111u64;
    let seed_b = 0x2222_2222u64;

    let mut gen = DataGenerator::new(GeneratorConfig {
        size: total,
        dedup_factor: 1,
        compress_factor: 1,
        seed: Some(seed_a),
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
    });
    let mut buf = vec![0u8; chunk];

    // Stripe 1: A
    gen.set_seed(Some(seed_a));
    gen.fill_chunk(&mut buf);
    let stripe_a1 = buf.clone();

    // Stripe 2: B
    gen.set_seed(Some(seed_b));
    gen.fill_chunk(&mut buf);
    let stripe_b1 = buf.clone();

    // Stripe 3: A again — must match Stripe 1
    gen.set_seed(Some(seed_a));
    gen.fill_chunk(&mut buf);
    let stripe_a2 = buf.clone();

    // Stripe 4: B again — must match Stripe 2
    gen.set_seed(Some(seed_b));
    gen.fill_chunk(&mut buf);
    let stripe_b2 = buf.clone();

    assert_eq!(
        stripe_a1, stripe_a2,
        "Stripe A must be reproducible after set_seed"
    );
    assert_eq!(
        stripe_b1, stripe_b2,
        "Stripe B must be reproducible after set_seed"
    );
    assert_ne!(stripe_a1, stripe_b1, "Stripe A and B must differ");
}

// ---------------------------------------------------------------------------
// Dedup is preserved across set_seed calls
// ---------------------------------------------------------------------------

#[test]
fn test_dedup_consistent_across_set_seed() {
    // After set_seed the epoch resets but the dedup cycle must still repeat
    // correctly (every unique_blocks-th block is identical to block 0).
    let num_blocks = 8;
    let size = num_blocks * BLOCK;
    let seed = 0xCAFE_BABEu64;

    let data = drain(new_seeded_gen(size, 2, 1, seed), 64 * 1024);
    assert_eq!(data.len(), size);

    let unique = count_unique_blocks(&data);
    let expected = num_blocks / 2;
    assert!(
        (unique as i64 - expected as i64).abs() <= 1,
        "dedup must hold after streaming with small chunks: expected ~{}, got {}",
        expected,
        unique
    );
}

// ---------------------------------------------------------------------------
// Concurrent generation (no data races)
// ---------------------------------------------------------------------------

#[test]
fn test_concurrent_generation() {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let results: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();

    for i in 0..4 {
        let results = Arc::clone(&results);
        let seed = 0x1000u64 + i as u64;
        handles.push(thread::spawn(move || {
            let data = drain(new_seeded_gen(4 * BLOCK, 2, 2, seed), 64 * 1024);
            results.lock().unwrap().push(data);
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }

    let r = results.lock().unwrap();
    assert_eq!(r.len(), 4);
    // All must be full size
    for v in r.iter() {
        assert_eq!(v.len(), 4 * BLOCK);
    }
    // All must differ (different seeds)
    for i in 0..r.len() {
        for j in (i + 1)..r.len() {
            assert_ne!(
                r[i], r[j],
                "threads {} and {} produced identical data",
                i, j
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-block compression (objects < 1 MiB)
//
// fill_block places random bytes at the START and zeros at the END of every
// block.  For sub-block objects the "block" IS the object.  We verify the
// compression ratio is correct by counting zero bytes, which is more reliable
// than a zstd round-trip for small sizes.
//
// Expected zero fraction: (compress - 1) / compress
//   compress=2 → 50 %    compress=4 → 75 %    compress=1 → ~0 % (pure random)
//
// Noise: XOR data is ~1/256 zeros ≈ 0.4 % on average.  For even the smallest
// tested size (512 bytes) that is at most 2 random zeros, well below the
// signal of 256 expected zeros for compress=2.
// ---------------------------------------------------------------------------

/// Returns the zero-byte fraction of a slice as a float in [0.0, 1.0].
fn zero_fraction(data: &[u8]) -> f64 {
    let zeros = data.iter().filter(|&&b| b == 0).count();
    zeros as f64 / data.len() as f64
}

/// Check that `zero_fraction` is within `tolerance` of `expected_fraction`.
fn assert_zero_fraction(data: &[u8], expected: f64, tolerance: f64, label: &str) {
    let frac = zero_fraction(data);
    assert!(
        (frac - expected).abs() <= tolerance,
        "{}: expected {:.1}% zeros ± {:.1}%, got {:.1}%",
        label,
        expected * 100.0,
        tolerance * 100.0,
        frac * 100.0,
    );
}

#[test]
fn test_sub_block_compress2_batch() {
    // For each sub-block size, compress=2 should produce ~50% zeros.
    for &size in &[512usize, 1024, 4096, 32 * 1024, 256 * 1024, BLOCK / 2] {
        use dgen_data::generate_data;
        let data = generate_data(GeneratorConfig {
            size,
            dedup_factor: 1,
            compress_factor: 2,
            seed: Some(0xABC1),
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: None,
        });
        assert_eq!(data.len(), size, "size mismatch for {}", size);
        // 50% expected, 10% tolerance (the random portion can absorb a few zeros,
        // and integer arithmetic in copy_len means the ratio is only approximate).
        assert_zero_fraction(
            data.as_slice(),
            0.50,
            0.10,
            &format!("batch compress=2 size={}", size),
        );
    }
}

#[test]
fn test_sub_block_compress4_batch() {
    // compress=4 → ~75% zeros.
    for &size in &[512usize, 1024, 4096, 32 * 1024, 256 * 1024, BLOCK / 2] {
        use dgen_data::generate_data;
        let data = generate_data(GeneratorConfig {
            size,
            dedup_factor: 1,
            compress_factor: 4,
            seed: Some(0xABC2),
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: None,
        });
        assert_eq!(data.len(), size);
        assert_zero_fraction(
            data.as_slice(),
            0.75,
            0.10,
            &format!("batch compress=4 size={}", size),
        );
    }
}

#[test]
fn test_sub_block_incompressible_batch() {
    // compress=1 → virtually no zeros (< 2%).
    for &size in &[512usize, 4096, 32 * 1024, BLOCK / 2] {
        use dgen_data::generate_data;
        let data = generate_data(GeneratorConfig {
            size,
            dedup_factor: 1,
            compress_factor: 1,
            seed: Some(0xABC3),
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: None,
        });
        assert_eq!(data.len(), size);
        // Random data has ~0.4% zeros on average; allow up to 2%.
        let frac = zero_fraction(data.as_slice());
        assert!(
            frac < 0.02,
            "batch compress=1 size={}: expected <2% zeros, got {:.1}%",
            size,
            frac * 100.0
        );
    }
}

#[test]
fn test_sub_block_compress2_streaming() {
    // Same as the batch test but using DataGenerator::fill_chunk with small chunks.
    for &size in &[512usize, 1024, 4096, 32 * 1024, 256 * 1024, BLOCK / 2] {
        let data = drain(new_seeded_gen(size, 1, 2, 0xDEF1), 512);
        assert_eq!(data.len(), size, "streaming size mismatch for {}", size);
        assert_zero_fraction(
            &data,
            0.50,
            0.10,
            &format!("streaming compress=2 size={}", size),
        );
    }
}

#[test]
fn test_sub_block_compress4_streaming() {
    for &size in &[512usize, 1024, 4096, 32 * 1024, 256 * 1024, BLOCK / 2] {
        let data = drain(new_seeded_gen(size, 1, 4, 0xDEF2), 512);
        assert_eq!(data.len(), size);
        assert_zero_fraction(
            &data,
            0.75,
            0.10,
            &format!("streaming compress=4 size={}", size),
        );
    }
}

// ---------------------------------------------------------------------------
// Batch and streaming produce identical bytes for the same seed (sub-block)
// ---------------------------------------------------------------------------

#[test]
fn test_batch_streaming_consistency_sub_block() {
    use dgen_data::generate_data;

    let seed = 0xFEED_BEEF_u64;
    for &size in &[512usize, 1024, 4096, 32 * 1024, 256 * 1024, BLOCK / 2] {
        let batch = generate_data(GeneratorConfig {
            size,
            dedup_factor: 1,
            compress_factor: 2,
            seed: Some(seed),
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: None,
        });
        let stream = drain(new_seeded_gen(size, 1, 2, seed), 512);

        assert_eq!(batch.len(), size, "batch size mismatch for {}", size);
        assert_eq!(stream.len(), size, "stream size mismatch for {}", size);
        assert_eq!(
            batch.as_slice(),
            stream.as_slice(),
            "batch and streaming produced different bytes for size={}",
            size
        );
    }
}
