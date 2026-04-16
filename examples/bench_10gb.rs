// examples/bench_10gb.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// 10-iteration per-object benchmark at 10 GB.
//
// Each iteration:
//   1. Print `free -h` to show available memory before generation.
//   2. Generate 10 GB via generate_data_simple() (new Rayon pool per call,
//      matching the large-object path in sai3-bench's data_gen_pool.rs).
//   3. Record elapsed time.
//   4. Print the result.
//   5. Explicitly drop the buffer with drop().
//   6. Print `free -h` to show memory returned to OS.
//
// Usage:
//   cargo run --release --example bench-10gb

use dgen_data::generate_data_simple;
use std::process::Command;
use std::time::Instant;

const SIZE: usize = 10 * 1024 * 1024 * 1024; // 10 GB
const RUNS: usize = 10;

fn free() {
    let out = Command::new("free")
        .arg("-h")
        .output()
        .expect("failed to run free");
    print!("{}", String::from_utf8_lossy(&out.stdout));
}

fn main() {
    println!("=== 10 GB per-object benchmark  ({RUNS} runs) ===");
    println!("  Calls generate_data_simple(10 GB) — new Rayon pool every call.");
    println!("  Buffer explicitly drop()'d after each timed call.");
    println!();

    let mut times = Vec::with_capacity(RUNS);

    for i in 1..=RUNS {
        println!("── Run {i}/{RUNS}  (before generation) ──");
        free();

        let t0 = Instant::now();
        let buf = generate_data_simple(SIZE, 1, 1);
        let elapsed = t0.elapsed().as_secs_f64();
        let gb_s = buf.len() as f64 / elapsed / 1e9;

        println!(
            "  → {:.3} s   {:.2} GB/s   ({} bytes)",
            elapsed,
            gb_s,
            buf.len()
        );
        times.push(gb_s);

        // Explicitly free the 10 GB buffer before the next iteration.
        drop(buf);

        println!("── Run {i}/{RUNS}  (after drop) ──");
        free();
        println!();
    }

    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = times.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    // median
    let mut sorted = times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];

    println!("=== Summary ===");
    println!("  Runs:    {RUNS}");
    for (i, v) in times.iter().enumerate() {
        println!("  Run {:2}:  {:.2} GB/s", i + 1, v);
    }
    println!();
    println!("  Min:     {min:.2} GB/s");
    println!("  Median:  {median:.2} GB/s");
    println!("  Avg:     {avg:.2} GB/s");
    println!("  Max:     {max:.2} GB/s");
}
