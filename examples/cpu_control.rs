// examples/cpu_control.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Example demonstrating CPU count and NUMA mode control

use dgen_data::{generate_data, GeneratorConfig, NumaMode};
use std::time::Instant;

fn main() {
    // Initialize tracing to see configuration info
    tracing_subscriber::fmt().with_env_filter("info").init();

    let size = 100 * 1024 * 1024; // 100 MiB

    println!("=== CPU Thread Control Examples ===\n");

    // Example 1: Use all available CPUs (default)
    println!("1. Using all available CPUs:");
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Auto,
        max_threads: None, // Use all cores
        numa_node: None,
    };

    let start = Instant::now();
    let _data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!("   Throughput: {:.2} GB/s\n", throughput);

    // Example 2: Limit to 4 threads
    println!("2. Limited to 4 threads:");
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Auto,
        max_threads: Some(4),
        numa_node: None,
    };

    let start = Instant::now();
    let _data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!("   Throughput: {:.2} GB/s\n", throughput);

    // Example 3: Single-threaded
    println!("3. Single thread:");
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Auto,
        max_threads: Some(1),
        numa_node: None,
    };

    let start = Instant::now();
    let _data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!(
        "   Throughput: {:.2} GB/s (baseline per-core)\n",
        throughput
    );

    println!("\n=== NUMA Mode Examples ===\n");

    // Example 4: Auto NUMA mode (default)
    println!("4. NUMA mode: Auto (enable on multi-node systems):");
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
    };

    let start = Instant::now();
    let _data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!("   Throughput: {:.2} GB/s\n", throughput);

    // Example 5: Force NUMA mode (for testing on UMA)
    println!("5. NUMA mode: Force (enable even on UMA systems):");
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Force,
        max_threads: None,
        numa_node: None,
    };

    let start = Instant::now();
    let _data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!("   Throughput: {:.2} GB/s\n", throughput);

    // Example 6: Disable NUMA mode
    println!("6. NUMA mode: Disabled (force disable optimizations):");
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Disabled,
        max_threads: None,
        numa_node: None,
    };

    let start = Instant::now();
    let _data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!("   Throughput: {:.2} GB/s\n", throughput);

    println!("\n=== Combined Configuration ===\n");

    // Example 7: 8 threads + Force NUMA
    println!("7. 8 threads + Force NUMA mode:");
    let config = GeneratorConfig {
        size,
        dedup_factor: 2,    // 2:1 dedup
        compress_factor: 3, // 3:1 compression
        block_size: None,
        seed: None,
        numa_mode: NumaMode::Force,
        max_threads: Some(8),
        numa_node: None,
    };

    let start = Instant::now();
    let data = generate_data(config);
    let elapsed = start.elapsed();
    let throughput = (size as f64) / elapsed.as_secs_f64() / 1e9;
    println!("   Generated {} bytes", data.len());
    println!("   Throughput: {:.2} GB/s", throughput);
    println!("   Note: Lower throughput due to compression overhead\n");
}
