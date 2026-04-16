// examples/small_object_bench.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Data generation speed benchmark across object size ranges.
//
// This benchmark measures two strategies:
//
//   BASELINE — generate_data_simple(size, dedup, compress)
//     One fresh 1 MB buffer is generated and allocated per call.
//     For objects < BLOCK_SIZE (1 MB) this wastes allocation effort.
//
//   ROLLING POOL — RollingPool::next_slice(size)
//     A 1 MB buffer is generated ONCE, then zero-copy Bytes::slice()
//     windows are handed out.  Refill only on exhaustion or config change.
//
// Run before implementing RollingPool for baseline numbers, then again
// after to see the improvement.
//
// Usage:
//   cargo build --release --example small_object_bench
//   ./target/release/examples/small_object_bench

use dgen_data::constants::BLOCK_SIZE;
use dgen_data::generate_data_simple;
use std::hint::black_box;
use std::time::Instant;

// Total usable output bytes per scenario
const TOTAL_BYTES: usize = 1024 * 1024 * 1024; // 1 GB

// Object sizes to benchmark: 64 KB, 1 MB, 1 GB
const SIZES: &[usize] = &[
    64 * 1024,          // 64 KB  — image-like small objects
    1024 * 1024,        // 1 MB   — exact BLOCK_SIZE
    1024 * 1024 * 1024, // 1 GB — single huge allocation
];

fn fmt_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{} GB", bytes / (1024 * 1024 * 1024))
    } else if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{} B", bytes)
    }
}

fn fmt_throughput(bytes: usize, secs: f64) -> String {
    let gb_s = bytes as f64 / secs / 1e9;
    if gb_s >= 1.0 {
        format!("{:.2} GB/s", gb_s)
    } else {
        format!("{:.0} MB/s", gb_s * 1000.0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Baseline: generate_data_simple() — current behaviour
// ─────────────────────────────────────────────────────────────────────────────

struct BenchResult {
    strategy: &'static str,
    object_size: usize,
    calls: usize,
    total_bytes: usize,
    elapsed_secs: f64,
    // Bytes actually allocated by dgen (≥ total_bytes when objects < BLOCK_SIZE)
    bytes_allocated: usize,
}

impl BenchResult {
    fn throughput_gb(&self) -> f64 {
        self.total_bytes as f64 / self.elapsed_secs / 1e9
    }
    fn waste_factor(&self) -> f64 {
        self.bytes_allocated as f64 / self.total_bytes as f64
    }
}

fn bench_simple(object_size: usize) -> BenchResult {
    // generate_data() enforces minimum of BLOCK_SIZE internally
    let effective_alloc = BLOCK_SIZE.max(object_size);
    // If object_size > TOTAL_BYTES, do 1 call
    let calls = (TOTAL_BYTES / object_size).max(1);

    // Warmup: 3 calls to warm instruction caches / rayon threads
    for _ in 0..3 {
        let mut buf = generate_data_simple(object_size, 1, 1);
        buf.truncate(object_size);
        black_box(buf.into_bytes());
    }

    let start = Instant::now();
    let mut total = 0usize;
    for _ in 0..calls {
        let mut buf = generate_data_simple(object_size, 1, 1);
        buf.truncate(object_size);
        let b = black_box(buf.into_bytes());
        total += b.len();
        // Drop b — simulates the PUT completing and releasing its buffer
    }
    let elapsed = start.elapsed().as_secs_f64();

    BenchResult {
        strategy: "generate_data_simple",
        object_size,
        calls,
        total_bytes: total,
        elapsed_secs: elapsed,
        bytes_allocated: calls * effective_alloc,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rolling pool: RollingPool::next_slice() — after implementation
// ─────────────────────────────────────────────────────────────────────────────

fn bench_rolling(object_size: usize) -> BenchResult {
    use dgen_data::RollingPool;

    let calls = (TOTAL_BYTES / object_size).max(1);

    let mut pool = RollingPool::new(1, 1);

    // Warmup
    for _ in 0..3 {
        black_box(pool.next_slice(object_size));
    }
    pool = RollingPool::new(1, 1); // reset after warmup

    let start = Instant::now();
    let mut total = 0usize;
    for _ in 0..calls {
        let b = black_box(pool.next_slice(object_size));
        total += b.len();
        // Drop b — Arc decrement only; no dealloc unless last reference
    }
    let elapsed = start.elapsed().as_secs_f64();

    // Actual allocations = number of pool refills × BLOCK_SIZE
    let refills = calls / (BLOCK_SIZE / object_size).max(1) + 1;
    let bytes_allocated = refills * BLOCK_SIZE;

    BenchResult {
        strategy: "RollingPool::next_slice",
        object_size,
        calls,
        total_bytes: total,
        elapsed_secs: elapsed,
        bytes_allocated,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Output formatting
// ─────────────────────────────────────────────────────────────────────────────

fn print_header() {
    println!(
        "\n{:<28} {:>8}  {:>10}  {:>10}  {:>10}  {:>7}",
        "Strategy", "Obj size", "Calls", "Output", "Throughput", "Alloc×"
    );
    println!("{}", "-".repeat(82));
}

fn print_result(r: &BenchResult) {
    println!(
        "{:<28} {:>8}  {:>10}  {:>10}  {:>10}  {:>7.1}x",
        r.strategy,
        fmt_size(r.object_size),
        r.calls,
        fmt_size(r.total_bytes),
        fmt_throughput(r.total_bytes, r.elapsed_secs),
        r.waste_factor(),
    );
}

fn print_separator() {
    println!("{}", "-".repeat(82));
}

// ─────────────────────────────────────────────────────────────────────────────
// main
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    println!("\n╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║         DATA GENERATION SPEED BENCHMARK  —  dgen-data                     ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!(
        "\n  Output target per scenario: {} total",
        fmt_size(TOTAL_BYTES)
    );
    println!("  Alloc× = ratio of bytes allocated by dgen vs. usable bytes returned");
    println!("  (Alloc× > 1.0 means data was generated but thrown away due to BLOCK_SIZE floor)");

    // ── BASELINE: generate_data_simple ────────────────────────────────────────
    println!("\n── BASELINE: generate_data_simple() ────────────────────────────────────────");
    print_header();

    let mut baseline_results = Vec::new();
    for &size in SIZES {
        let r = bench_simple(size);
        print_result(&r);
        baseline_results.push(r);
    }
    print_separator();

    // ── ROLLING POOL: RollingPool::next_slice() ───────────────────────────────
    {
        println!("\n── ROLLING POOL: RollingPool::next_slice() ──────────────────────────────────");
        print_header();

        let mut pool_results = Vec::new();
        for &size in SIZES {
            let r = bench_rolling(size);
            print_result(&r);
            pool_results.push(r);
        }
        print_separator();

        // ── COMPARISON ────────────────────────────────────────────────────────
        println!("\n── IMPROVEMENT SUMMARY ──────────────────────────────────────────────────────");
        println!(
            "{:<12}  {:>18}  {:>18}  {:>10}",
            "Object size", "Baseline", "RollingPool", "Speedup"
        );
        println!("{}", "-".repeat(66));
        for (b, p) in baseline_results.iter().zip(pool_results.iter()) {
            let speedup = p.throughput_gb() / b.throughput_gb();
            println!(
                "{:<12}  {:>18}  {:>18}  {:>9.2}x",
                fmt_size(b.object_size),
                fmt_throughput(b.total_bytes, b.elapsed_secs),
                fmt_throughput(p.total_bytes, p.elapsed_secs),
                speedup,
            );
        }
        println!("{}", "-".repeat(66));
    }

    println!();
}
