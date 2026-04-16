// examples/rust_speed_probe.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Rust-native speed probe called by the Python benchmark table.
//
// Measures RollingPool::next_slice() throughput at a given object size
// with a time-based warmup phase that drives the CPU to its steady-state
// turbo frequency and primes the OS allocator before measurement begins.
// This gives correct numbers regardless of how cold the subprocess starts.
//
// Warmup strategy:
//   - Run for at least WARMUP_MS milliseconds (always ≥ 1 iteration).
//   - This forces the CPU to max turbo frequency and gets Rayon thread pool
//     lifecycle overhead out of the measurement window.
//
// Measurement strategy:
//   - Run for at least MEASURE_MS milliseconds AND at least the number of
//     calls implied by total_bytes/obj_bytes.  Both conditions must be met
//     before the loop exits, so large objects always get ≥ 1 measured call.
//
// Usage (after `cargo build --release --example rust-speed-probe`):
//   ./target/release/examples/rust-speed-probe <obj_bytes> [total_bytes]
//
// Output (single line, machine-readable):
//   <bytes_generated> <elapsed_secs>

use dgen_data::RollingPool;
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Minimum warmup duration — enough to ramp CPU turbo frequency and prime
/// the OS allocator.  For large objects (> 1 GB) a single iteration likely
/// already exceeds this; the loop will end after that one iteration.
const WARMUP_MS: u64 = 500;

/// Minimum measurement duration — ensures statistical stability even for
/// fast small-object workloads.
const MEASURE_MS: u64 = 1000;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let obj_bytes: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: rust-speed-probe <obj_bytes> [total_bytes]");

    // The minimum number of calls the caller requested.
    // Default: enough for 1 GiB; for obj_bytes > 1 GiB this is 1.
    let total_bytes: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024 * 1024 * 1024);

    let min_calls: usize = (total_bytes / obj_bytes).max(1);

    let mut pool = RollingPool::new(1, 1);

    // ── Warmup phase ──────────────────────────────────────────────────────────
    // Run until at least WARMUP_MS has elapsed.  For small objects this is
    // millions of iterations; for large objects (e.g. 10 GB at ~9 GB/s) a
    // single iteration takes ~1.1 s which already exceeds the 500 ms target.
    let warmup_deadline = Instant::now() + Duration::from_millis(WARMUP_MS);
    loop {
        black_box(pool.next_slice(obj_bytes));
        if Instant::now() >= warmup_deadline {
            break;
        }
    }

    // ── Measurement phase ─────────────────────────────────────────────────────
    // Continue until BOTH conditions are satisfied:
    //   1. At least min_calls iterations completed.
    //   2. At least MEASURE_MS elapsed (statistical stability for fast sizes).
    let measure_deadline = Instant::now() + Duration::from_millis(MEASURE_MS);
    let measure_start = Instant::now();
    let mut generated: usize = 0;
    let mut iters: usize = 0;

    loop {
        let buf = pool.next_slice(obj_bytes);
        generated += black_box(buf.len());
        iters += 1;
        if iters >= min_calls && Instant::now() >= measure_deadline {
            break;
        }
    }

    let elapsed = measure_start.elapsed().as_secs_f64();

    // Single line, two numbers: bytes generated, elapsed seconds
    println!("{} {:.9}", generated, elapsed);
}
