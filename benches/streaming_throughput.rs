// Benchmark for streaming data generation throughput
// Tests different block sizes to find optimal performance

use dgen_data::generator::{DataGenerator, GeneratorConfig, NumaMode};
use std::time::Instant;

const TEST_SIZE: usize = 100 * 1024 * 1024 * 1024; // 100 GB
const WARMUP_SIZE: usize = 1024 * 1024 * 1024; // 1 GB
const ITERATIONS: usize = 5;
const CHUNK_SIZE: usize = 32 * 1024 * 1024; // 32 MB chunks for streaming

fn benchmark_block_size(block_size: usize) {
    println!("\n{}", "=".repeat(80));
    println!("Testing block_size = {} MB", block_size / (1024 * 1024));
    println!("{}", "=".repeat(80));

    // Warmup
    println!(
        "Warming up with {} GB...",
        WARMUP_SIZE / (1024 * 1024 * 1024)
    );
    let config = GeneratorConfig {
        size: WARMUP_SIZE,
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: Some(block_size),
        seed: None,
    };

    let mut gen = DataGenerator::new(config);
    let mut buffer = vec![0u8; CHUNK_SIZE];

    while !gen.is_complete() {
        gen.fill_chunk(&mut buffer);
    }

    println!("Warmup complete. Starting benchmark...");
    println!(
        "Generating {} GB per run (streaming mode)",
        TEST_SIZE / (1024 * 1024 * 1024)
    );
    println!();

    let mut run_times = Vec::new();

    for i in 1..=ITERATIONS {
        // Create new generator for each run
        let config = GeneratorConfig {
            size: TEST_SIZE,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: Some(block_size),
            seed: None,
        };

        let mut gen = DataGenerator::new(config);
        let mut buffer = vec![0u8; CHUNK_SIZE];

        let start = Instant::now();

        while !gen.is_complete() {
            gen.fill_chunk(&mut buffer);
        }

        let duration = start.elapsed();
        let duration_secs = duration.as_secs_f64();
        let throughput = (TEST_SIZE as f64 / 1024.0 / 1024.0 / 1024.0) / duration_secs;

        run_times.push(duration_secs);
        println!(
            "Run {:02}: {:.4} seconds | {:.2} GB/s",
            i, duration_secs, throughput
        );
    }

    let avg_duration = run_times.iter().sum::<f64>() / ITERATIONS as f64;
    let avg_throughput = (TEST_SIZE as f64 / 1024.0 / 1024.0 / 1024.0) / avg_duration;

    println!(
        "AVERAGE: {:.4} seconds | {:.2} GB/s",
        avg_duration, avg_throughput
    );
}

fn main() {
    println!("RUST STREAMING THROUGHPUT BENCHMARK");
    println!("Test size: {} GB", TEST_SIZE / (1024 * 1024 * 1024));
    println!("Iterations: {}", ITERATIONS);
    println!("Chunk size: {} MB", CHUNK_SIZE / (1024 * 1024));
    println!();

    // Get system info
    #[cfg(feature = "numa")]
    {
        use dgen_data::numa::NumaTopology;
        if let Ok(topology) = NumaTopology::detect() {
            println!("System Configuration:");
            println!("  NUMA nodes: {}", topology.num_nodes);
            println!("  Physical cores: {}", topology.physical_cores);
            println!("  Logical CPUs: {}", topology.logical_cpus);
            println!("  Deployment: {}", topology.deployment_type());
            println!();
        }
    }

    #[cfg(not(feature = "numa"))]
    {
        println!("System Configuration:");
        println!("  Physical cores: {}", num_cpus::get_physical());
        println!("  Logical CPUs: {}", num_cpus::get());
        println!();
    }

    // Test different block sizes
    // With 32 MB chunk_size, we need smaller blocks to allow parallel processing
    // Smaller blocks = more blocks per chunk = better parallelism!
    let block_sizes = vec![
        256 * 1024,  // 256 KB (128 blocks per 32 MB chunk)
        512 * 1024,  // 512 KB (64 blocks per 32 MB chunk)
        1024 * 1024, // 1 MB (32 blocks per 32 MB chunk)
    ];

    for block_size in block_sizes {
        benchmark_block_size(block_size);
    }

    println!("\n{}", "=".repeat(80));
    println!("BENCHMARK COMPLETE");
    println!("{}", "=".repeat(80));
}
