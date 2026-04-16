// examples/speed_table.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Two-column pure-Rust speed table comparing dgen-data's two usage patterns:
//
//   Per-object  — RollingPool::next_slice()
//                 ≤ 1 MB: zero-copy slice from a pre-generated 1 MB block
//                 > 1 MB: generate_data_simple() — new Rayon thread pool every call
//                 Models a server/handler that generates one object per request.
//
//   Streaming   — DataGenerator::fill_chunk() with a fixed 32 MB buffer
//                 Thread pool created ONCE in DataGenerator::new() and reused
//                 for every fill_chunk() call within this run.
//                 Models bulk benchmark tools, data pipelines, and any workload
//                 that continuously generates large volumes of data.
//
// Usage:
//   cargo run --release --example speed-table

use dgen_data::generator::{DataGenerator, GeneratorConfig, NumaMode};
use dgen_data::RollingPool;
use std::hint::black_box;
use std::time::{Duration, Instant};

// ── Timing constants ─────────────────────────────────────────────────────────

/// Warmup for the per-object column: drives CPU turbo and warms the pool.
const PO_WARMUP: Duration = Duration::from_millis(500);
/// Measurement window for the per-object column.
const PO_MEASURE: Duration = Duration::from_millis(2000);
/// Minimum iteration count for the per-object column.
/// For large objects (e.g. 10 GB ≈ 500 ms each) the time window alone
/// captures only 3–4 cold-start calls.  Enforcing MIN_ITERS ensures ≥ 10
/// steady-state calls so the average is representative.
const PO_MIN_ITERS: usize = 10;

/// Chunk size passed to DataGenerator::fill_chunk() — matches the optimum
/// found in benches/streaming_throughput.rs.
const STREAM_CHUNK: usize = 32 * 1024 * 1024; // 32 MB
/// Warmup volume for the streaming column: drives CPU and warms the pool.
const STREAM_WARMUP_TOTAL: usize = 1024 * 1024 * 1024; // 1 GB
/// Minimum data volume for the streaming measurement.
const STREAM_MEASURE_MIN: usize = 10 * 1024 * 1024 * 1024; // 10 GB
/// Maximum data volume for the streaming measurement (caps run-time).
const STREAM_MEASURE_MAX: usize = 20 * 1024 * 1024 * 1024; // 20 GB

// ── Rows ──────────────────────────────────────────────────────────────────────

struct Row {
    label: &'static str,
    obj_bytes: usize,
}

const SIZES: &[Row] = &[
    Row {
        label: "64 B  ",
        obj_bytes: 64,
    },
    Row {
        label: "512 B ",
        obj_bytes: 512,
    },
    Row {
        label: "4 KB  ",
        obj_bytes: 4 * 1024,
    },
    Row {
        label: "64 KB ",
        obj_bytes: 64 * 1024,
    },
    Row {
        label: "1 MB  ",
        obj_bytes: 1024 * 1024,
    },
    Row {
        label: "10 MB ",
        obj_bytes: 10 * 1024 * 1024,
    },
    Row {
        label: "100 MB",
        obj_bytes: 100 * 1024 * 1024,
    },
    Row {
        label: "1 GB  ",
        obj_bytes: 1024 * 1024 * 1024,
    },
    Row {
        label: "10 GB ",
        obj_bytes: 10 * 1024 * 1024 * 1024,
    },
];

// ── Helpers ──────────────────────────────────────────────────────────────────

fn fmt_tput(bytes: usize, secs: f64) -> String {
    let gb_s = bytes as f64 / secs / 1e9;
    if gb_s >= 1.0 {
        format!("{:.2} GB/s", gb_s)
    } else {
        format!("{:.0} MB/s", gb_s * 1000.0)
    }
}

fn make_cfg(size: usize) -> GeneratorConfig {
    GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
        seed: None,
    }
}

// ── Column 1: Per-object (RollingPool) ───────────────────────────────────────

/// Benchmark the per-object API via RollingPool::next_slice().
///
/// For sizes ≤ 1 MB the pool hands out zero-copy Bytes slices from a
/// pre-generated 1 MB block; a new block is generated only on exhaustion.
///
/// For sizes > 1 MB the call bypasses the pool and invokes
/// generate_data_simple(), which creates a fresh Rayon thread pool per call
/// then drops it when the function returns.  This is the cost you pay if you
/// generate one large object at a time without reusing the generator.
fn bench_per_object(obj_bytes: usize) -> (usize, f64) {
    let mut pool = RollingPool::new(1, 1);

    // Warmup: sustain CPU turbo and pre-fill the pool.
    let wd = Instant::now() + PO_WARMUP;
    loop {
        black_box(pool.next_slice(obj_bytes));
        if Instant::now() >= wd {
            break;
        }
    }

    // Measurement: accumulate per-call generation time only.
    // The buffer is dropped AFTER the timer stops, so munmap cost is excluded.
    // Run until BOTH ≥ PO_MIN_ITERS calls AND the wall-clock window has elapsed.
    let wall_deadline = Instant::now() + PO_MEASURE;
    let mut generated: usize = 0;
    let mut iters: usize = 0;
    let mut gen_secs = 0f64;

    loop {
        let t0 = Instant::now();
        let buf = pool.next_slice(obj_bytes);
        gen_secs += t0.elapsed().as_secs_f64(); // stop timer before drop

        generated += black_box(buf.len());
        iters += 1;
        drop(buf); // munmap happens here, untimed

        if iters >= PO_MIN_ITERS && Instant::now() >= wall_deadline {
            break;
        }
    }

    (generated, gen_secs)
}

// ── Column 2: Streaming (DataGenerator) ──────────────────────────────────────

/// Benchmark the streaming API via DataGenerator::fill_chunk().
///
/// A single DataGenerator is created for the full measurement volume.
/// Its internal Rayon thread pool is created once in DataGenerator::new()
/// and reused for every fill_chunk() call — no per-call pool overhead.
///
/// The 32 MB output buffer is allocated once before the loop.  Because it
/// fits in DRAM and the OS can reuse the same physical pages, the benchmark
/// measures sustained CPU/memory-bus throughput rather than allocation cost.
///
/// This is the appropriate model for benchmark tools, data pipelines, and any
/// workload generating many gigabytes of data in one continuous pass.
fn bench_streaming(obj_bytes: usize) -> (usize, f64) {
    // Choose measurement volume: at least STREAM_MEASURE_MIN (so there are
    // enough fill_chunk() calls to amortize setup), capped at STREAM_MEASURE_MAX
    // to keep total benchmark time reasonable.
    let measure_total = STREAM_MEASURE_MIN
        .max(obj_bytes.saturating_mul(4))
        .min(STREAM_MEASURE_MAX);

    // Warmup: a separate DataGenerator to drive CPU turbo.
    {
        let warmup_total = STREAM_WARMUP_TOTAL.min(measure_total);
        let mut wgen = DataGenerator::new(make_cfg(warmup_total));
        let mut buf = vec![0u8; STREAM_CHUNK];
        while !wgen.is_complete() {
            black_box(wgen.fill_chunk(&mut buf));
        }
    }

    // Measurement: new DataGenerator so the thread pool starts fresh.
    let mut gen = DataGenerator::new(make_cfg(measure_total));
    let mut buf = vec![0u8; STREAM_CHUNK];

    let t0 = Instant::now();
    while !gen.is_complete() {
        gen.fill_chunk(&mut buf);
    }

    (measure_total, t0.elapsed().as_secs_f64())
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let ncpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let sep = "─".repeat(62);

    println!();
    println!("  dgen-data speed table  ({ncpus} logical CPUs)  — Pure Rust, no PyO3");
    println!();
    println!("  Per-object  — RollingPool::next_slice()");
    println!("    ≤ 1 MB  zero-copy slice from a pre-generated 1 MB pool block");
    println!("    > 1 MB  generate_data_simple(): new Rayon pool created per call");
    println!();
    println!("  Streaming   — DataGenerator::fill_chunk(), 32 MB chunks");
    println!("    Thread pool created ONCE, reused for every fill_chunk() call.");
    println!("    No per-call pool startup; all cores busy for the full run.");
    println!();
    println!("  Per-object warmup: {PO_WARMUP:?}  measurement: ≥ {PO_MEASURE:?} per row");
    println!(
        "  Streaming  warmup: {} GB  measurement: {}-{} GB per row",
        STREAM_WARMUP_TOTAL / (1 << 30),
        STREAM_MEASURE_MIN / (1 << 30),
        STREAM_MEASURE_MAX / (1 << 30),
    );
    println!();
    println!(
        "  {:<8}  {:>14}  {:>14}",
        "Object", "Per-object", "Streaming"
    );
    println!("  {sep}");

    for row in SIZES {
        // Per-object column
        let (po_bytes, po_secs) = bench_per_object(row.obj_bytes);
        // Streaming column
        let (st_bytes, st_secs) = bench_streaming(row.obj_bytes);

        println!(
            "  {:<8}  {:>14}  {:>14}",
            row.label,
            fmt_tput(po_bytes, po_secs),
            fmt_tput(st_bytes, st_secs),
        );
    }

    println!();
    println!("  Note: Streaming throughput is independent of \"object size\" — the");
    println!("  DataGenerator always fills 32 MB chunks regardless.  The per-row");
    println!("  streaming numbers will be nearly identical across all sizes; any");
    println!("  variation is measurement noise.  The important comparison is the");
    println!("  per-object vs streaming columns for large (> 1 MB) objects, where");
    println!("  the per-call Rayon pool overhead is visible.");
    println!();
}
