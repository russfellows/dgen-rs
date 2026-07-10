// SPDX-License-Identifier: Apache-2.0 OR MIT
// SPDX-FileCopyrightText: 2026 Russ Fellows <russ.fellows@gmail.com>
//
// Correctness tests for the numeric-distribution primitives added for
// mlcommons/storage#625 / dgen-rs docs/DESIGN_NUMERIC_DISTRIBUTIONS.md:
//   - fill_uniform_f32: parallel uniform float32 generation via IEEE-754
//     bit-masking of the existing Xoshiro256++ random byte stream
//   - normalize_rows_f32: parallel in-place L2 row normalization
//   - generate_uniform_vectors_data: fused generate+normalize
//
// RED-then-GREEN: this file is written BEFORE the implementation exists, so
// `cargo test --test test_numeric_distributions` must fail to COMPILE
// (unresolved imports) against unmodified generator.rs. That compile
// failure is the RED signal — confirmed before any implementation lands.

use dgen_data::{
    fill_uniform_f32, generate_uniform_vectors_data, normalize_rows_f32, GeneratorConfig,
};

fn cfg(max_threads: Option<usize>) -> GeneratorConfig {
    GeneratorConfig {
        size: 0, // unused by these functions — buffer length drives output size
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: dgen_data::NumaMode::Auto,
        max_threads,
        numa_node: None,
        block_size: None,
        seed: None,
    }
}

fn as_f32_slice(buf: &[u8]) -> &[f32] {
    assert_eq!(buf.len() % 4, 0);
    // SAFETY: test-only, buf is 4-byte-aligned Vec<u8> backing storage in
    // practice for these tests (Vec<u8> from a freshly allocated Vec).
    unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const f32, buf.len() / 4) }
}

// ---------------------------------------------------------------------------
// fill_uniform_f32
// ---------------------------------------------------------------------------

#[test]
fn fill_uniform_f32_all_finite_and_in_range() {
    let n = 100_000;
    let mut buf = vec![0u8; n * 4];
    fill_uniform_f32(&mut buf, 0.0, 1.0, &cfg(None));
    let floats = as_f32_slice(&buf);
    assert_eq!(floats.len(), n);
    for &v in floats {
        assert!(v.is_finite(), "found non-finite value: {v}");
        assert!((0.0..1.0).contains(&v), "value {v} outside [0.0, 1.0)");
    }
}

#[test]
fn fill_uniform_f32_respects_low_high_range() {
    let n = 50_000;
    let mut buf = vec![0u8; n * 4];
    fill_uniform_f32(&mut buf, -5.0, 5.0, &cfg(None));
    let floats = as_f32_slice(&buf);
    for &v in floats {
        assert!(v.is_finite());
        assert!((-5.0..5.0).contains(&v), "value {v} outside [-5.0, 5.0)");
    }
}

#[test]
fn fill_uniform_f32_basic_uniformity_sanity() {
    // Coarse chi-squared-style sanity check: with 200k samples in [0,1)
    // split into 10 bins, no bin should be wildly over/under-represented.
    let n = 200_000;
    let mut buf = vec![0u8; n * 4];
    fill_uniform_f32(&mut buf, 0.0, 1.0, &cfg(None));
    let floats = as_f32_slice(&buf);

    let mut bins = [0u32; 10];
    for &v in floats {
        let bin = ((v * 10.0) as usize).min(9);
        bins[bin] += 1;
    }
    let expected = n as f64 / 10.0;
    for (i, &count) in bins.iter().enumerate() {
        let ratio = count as f64 / expected;
        assert!(
            (0.85..1.15).contains(&ratio),
            "bin {i} count {count} deviates >15% from expected {expected} (ratio {ratio})"
        );
    }
}

#[test]
fn fill_uniform_f32_uses_full_generation_engine_scales_with_threads() {
    // Not a strict timing assertion (kept out of correctness tests per
    // design doc §8) -- just confirms both thread counts produce valid,
    // differently-seeded output without panicking or hanging.
    let n = 10_000;
    let mut buf1 = vec![0u8; n * 4];
    let mut buf2 = vec![0u8; n * 4];
    fill_uniform_f32(&mut buf1, 0.0, 1.0, &cfg(Some(1)));
    fill_uniform_f32(&mut buf2, 0.0, 1.0, &cfg(Some(4)));
    assert!(as_f32_slice(&buf1).iter().all(|v| v.is_finite()));
    assert!(as_f32_slice(&buf2).iter().all(|v| v.is_finite()));
}

// ---------------------------------------------------------------------------
// normalize_rows_f32
// ---------------------------------------------------------------------------

#[test]
fn normalize_rows_f32_unit_l2_norm() {
    let rows = 1_000;
    let dim = 128;
    let mut buf = vec![0u8; rows * dim * 4];
    fill_uniform_f32(&mut buf, 0.0, 1.0, &cfg(None));
    normalize_rows_f32(&mut buf, dim, None);

    let floats = as_f32_slice(&buf);
    for r in 0..rows {
        let row = &floats[r * dim..(r + 1) * dim];
        let norm: f32 = row.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "row {r} L2 norm {norm} not ~1.0");
    }
}

#[test]
fn normalize_rows_f32_zero_row_left_unchanged_not_nan() {
    let dim = 8;
    let mut buf = vec![0u8; dim * 4]; // all-zero row (buf initialized to 0)
    normalize_rows_f32(&mut buf, dim, None);
    let floats = as_f32_slice(&buf);
    for &v in floats {
        assert_eq!(v, 0.0, "zero row must stay zero, not become NaN");
        assert!(!v.is_nan());
    }
}

#[test]
fn normalize_rows_f32_parallel_matches_sequential_result() {
    // Same input, different max_threads -- normalization result must be
    // identical (thread count must not change the math).
    let rows = 2_000;
    let dim = 64;
    let mut buf_seq = vec![0u8; rows * dim * 4];
    fill_uniform_f32(&mut buf_seq, 0.0, 1.0, &cfg(None));
    let mut buf_par = buf_seq.clone();

    normalize_rows_f32(&mut buf_seq, dim, Some(1));
    normalize_rows_f32(&mut buf_par, dim, Some(8));

    let seq = as_f32_slice(&buf_seq);
    let par = as_f32_slice(&buf_par);
    for i in 0..seq.len() {
        assert!(
            (seq[i] - par[i]).abs() < 1e-6,
            "index {i}: seq={} par={}",
            seq[i],
            par[i]
        );
    }
}

// ---------------------------------------------------------------------------
// generate_uniform_vectors_data (fused)
// ---------------------------------------------------------------------------

#[test]
fn generate_uniform_vectors_data_normalized_matches_separate_calls() {
    let rows = 500;
    let dim = 96;
    let seed = 42;

    let mut cfg_a = cfg(None);
    cfg_a.seed = Some(seed);
    let fused = generate_uniform_vectors_data(rows, dim, 0.0, 1.0, true, &cfg_a);

    let mut cfg_b = cfg(None);
    cfg_b.seed = Some(seed);
    let mut separate = vec![0u8; rows * dim * 4];
    fill_uniform_f32(&mut separate, 0.0, 1.0, &cfg_b);
    normalize_rows_f32(&mut separate, dim, None);

    assert_eq!(fused.as_slice(), separate.as_slice(),
        "fused generate+normalize must be byte-for-byte equivalent to calling the two steps separately with the same seed");
}

#[test]
fn generate_uniform_vectors_data_normalize_false_skips_normalization() {
    let rows = 200;
    let dim = 32;
    let data = generate_uniform_vectors_data(rows, dim, 0.0, 1.0, false, &cfg(None));
    let floats = as_f32_slice(data.as_slice());
    // At least one row should NOT be unit-normalized (astronomically
    // unlikely for freshly generated uniform data to already be unit-norm).
    let row0 = &floats[0..dim];
    let norm: f32 = row0.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() > 1e-3,
        "normalize=false must leave rows un-normalized"
    );
}

#[test]
fn generate_uniform_vectors_data_all_rows_normalized_when_requested() {
    let rows = 300;
    let dim = 64;
    let data = generate_uniform_vectors_data(rows, dim, 0.0, 1.0, true, &cfg(None));
    let floats = as_f32_slice(data.as_slice());
    for r in 0..rows {
        let row = &floats[r * dim..(r + 1) * dim];
        let norm: f32 = row.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "row {r} norm {norm} not ~1.0");
    }
}
