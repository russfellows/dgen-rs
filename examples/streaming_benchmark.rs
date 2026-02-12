// examples/streaming_benchmark.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Streaming benchmark - generates data in chunks like real storage workloads
//!
//! This measures pure generation speed without Python overhead.
//! Use this to verify generation can exceed storage bandwidth (80+ GB/s target).

use dgen_rs::{DataGenerator, GeneratorConfig, NumaMode};
use std::time::Instant;

fn format_throughput(bytes: usize, duration_secs: f64) -> String {
    let gb_per_sec = (bytes as f64) / duration_secs / 1e9;
    format!("{:.2} GB/s", gb_per_sec)
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{} GB", bytes / (1024 * 1024 * 1024))
    } else if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else {
        format!("{} bytes", bytes)
    }
}

fn main() {
    // Initialize logging
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("\n=================================================================");
    println!("STREAMING DATA GENERATION BENCHMARK");
    println!("=================================================================");
    println!("Goal: Measure pure generation speed for storage workloads");
    println!("Target: > 80 GB/s (to not bottleneck storage)");
    println!("=================================================================\n");

    // Test configuration
    let total_size = 100 * 1024 * 1024 * 1024; // 100 GB
    let chunk_size = 64 * 1024 * 1024; // 64 MB chunks
    let iterations = 3;

    println!("Configuration:");
    println!("  Total size per run: {}", format_size(total_size));
    println!("  Chunk size: {}", format_size(chunk_size));
    println!("  Iterations: {}", iterations);
    println!("  Threads: All available (auto-detect)");
    println!("  NUMA mode: Auto\n");

    let mut run_times = Vec::new();

    for run in 1..=iterations {
        // Create streaming generator
        let config = GeneratorConfig {
            size: total_size,
            dedup_factor: 1,
            compress_factor: 1,
            block_size: None,
            seed: None,
            numa_mode: NumaMode::Auto,
            max_threads: None, // Use all cores
            numa_node: None,
        };

        let mut generator = DataGenerator::new(config);

        // Pre-allocate reusable buffer (zero-copy - reused across chunks)
        let mut buffer = vec![0u8; chunk_size];

        let start = Instant::now();
        let mut total_generated = 0;
        let mut chunks = 0;

        // Stream through all data
        while !generator.is_complete() {
            let nbytes = generator.fill_chunk(&mut buffer);
            if nbytes == 0 {
                break;
            }
            total_generated += nbytes;
            chunks += 1;

            // In real usage, you would write buffer[..nbytes] to storage here
            // e.g., file.write_all(&buffer[..nbytes])?;
        }

        let elapsed = start.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();
        let throughput = format_throughput(total_generated, elapsed_secs);

        run_times.push(elapsed_secs);

        println!(
            "Run {:02}: {:.4}s | {} | {} chunks",
            run, elapsed_secs, throughput, chunks
        );
    }

    // Calculate statistics
    let avg_time = run_times.iter().sum::<f64>() / run_times.len() as f64;
    let avg_throughput = format_throughput(total_size, avg_time);

    println!("\n{}", "-".repeat(65));
    println!("RESULTS:");
    println!("  Average duration: {:.4}s", avg_time);
    println!("  Average throughput: {}", avg_throughput);
    println!("  Total data: {}", format_size(total_size * iterations));
    println!("{}", "-".repeat(65));

    // Extract numeric throughput for comparison
    let throughput_value = (total_size as f64) / avg_time / 1e9;

    println!("\nANALYSIS:");
    if throughput_value >= 80.0 {
        println!(
            "  ✅ EXCELLENT: {} exceeds 80 GB/s storage target",
            avg_throughput
        );
        println!("     Generation will NOT bottleneck storage");
    } else if throughput_value >= 50.0 {
        println!(
            "  ⚠️  GOOD: {} is decent but below 80 GB/s target",
            avg_throughput
        );
        println!("     May bottleneck very fast storage");
    } else {
        println!(
            "  ❌ SLOW: {} is significantly below 80 GB/s",
            avg_throughput
        );
        println!("     Will bottleneck storage - needs investigation");
    }

    println!("\nNOTES:");
    println!("  - This is NATIVE RUST (no Python overhead)");
    println!("  - Buffer is REUSED (zero-copy across chunks)");
    println!("  - Parallel generation across all cores");
    println!("  - In real workload, add write time: total_time = gen_time + write_time");
    println!("\nNEXT STEPS:");
    println!("  1. If throughput > 80 GB/s: Generation is NOT the bottleneck");
    println!("  2. Test with actual storage writes (O_DIRECT + io_uring)");
    println!("  3. Compare: storage_write_speed vs generation_speed");
    println!("=================================================================\n");
}
