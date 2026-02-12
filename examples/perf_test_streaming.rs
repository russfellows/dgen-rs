use dgen_data::{DataGenerator, GeneratorConfig, NumaMode};
use std::time::Instant;

fn test_chunk_size(size: usize, chunk_size: usize) -> f64 {
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

    let mut gen = DataGenerator::new(config);
    let mut buffer = vec![0u8; chunk_size];
    let mut total_bytes = 0u64;

    let start = Instant::now();

    while !gen.is_complete() {
        let nbytes = gen.fill_chunk(&mut buffer);
        if nbytes == 0 {
            break;
        }
        total_bytes += nbytes as u64;
    }

    let elapsed = start.elapsed().as_secs_f64();
    let gb = total_bytes as f64 / 1e9;
    gb / elapsed
}

fn main() {
    let size = 100 * 1024 * 1024 * 1024; // 100 GB total

    // Test chunk sizes from 4 MB to 512 MB
    let chunk_sizes_mb = vec![4, 8, 16, 32, 64, 128, 256, 512];

    println!("Testing chunk size optimization (100 GB total)");
    println!("------------------------------------------------------------");
    println!(
        "{:>12} | {:>15} | {:>10}",
        "Chunk Size", "Throughput", "% of Best"
    );
    println!("------------------------------------------------------------");

    let mut results = Vec::new();

    for &size_mb in &chunk_sizes_mb {
        let chunk_size = size_mb * 1024 * 1024;
        let throughput = test_chunk_size(size, chunk_size);
        results.push((size_mb, throughput));
        println!("{:>9} MB | {:>12.2} GB/s |", size_mb, throughput);
    }

    // Find best and compute percentages
    let best_throughput = results
        .iter()
        .map(|(_, t)| t)
        .fold(0.0f64, |a, &b| a.max(b));

    println!("------------------------------------------------------------");
    println!("\nRESULTS WITH PERCENTAGE OF BEST:");
    println!("------------------------------------------------------------");
    println!(
        "{:>12} | {:>15} | {:>10}",
        "Chunk Size", "Throughput", "% of Best"
    );
    println!("------------------------------------------------------------");

    for (size_mb, throughput) in results {
        let percentage = (throughput / best_throughput) * 100.0;
        let marker = if throughput == best_throughput {
            " ← BEST"
        } else {
            ""
        };
        println!(
            "{:>9} MB | {:>12.2} GB/s | {:>9.1}%{}",
            size_mb, throughput, percentage, marker
        );
    }

    println!("------------------------------------------------------------");
}
