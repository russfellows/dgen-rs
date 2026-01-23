use dgen_rs::{DataGenerator, GeneratorConfig, NumaMode};
use std::time::Instant;

fn main() {
    println!("NUMA Thread Pinning & Memory Locality Test");
    println!("==========================================\n");
    
    let size = 100 * 1024 * 1024 * 1024; // 100 GB (matching original benchmark)
    let iterations = 3;
    
    // Test 1: NUMA Disabled (no thread pinning)
    println!("Test 1: NUMA Disabled (no thread pinning)");
    run_test_with_chunk_size(size, iterations, NumaMode::Disabled, None, 32 * 1024 * 1024);
    println!();
    
    // Test 2: NUMA Auto (enable on multi-socket systems)
    println!("Test 2: NUMA Auto (enable if multi-socket detected)");
    run_test_with_chunk_size(size, iterations, NumaMode::Auto, None, 32 * 1024 * 1024);
    println!();
    
    // Test 3: NUMA Force (always enable thread pinning)
    println!("Test 3: NUMA Force (always pin threads)");
    run_test_with_chunk_size(size, iterations, NumaMode::Force, None, 32 * 1024 * 1024);
    println!();
    
    // Test 4: NUMA Force with half the threads (physical cores only)
    println!("Test 4: NUMA Force with half threads (physical cores only)");
    let half_threads = num_cpus::get() / 2;
    run_test_with_chunk_size(size, iterations, NumaMode::Force, Some(half_threads), 32 * 1024 * 1024);
    println!();
    
    // Test 5: 64 MB chunks (optimal buffer size)
    println!("Test 5: 64 MB chunks (optimal buffer size from testing)");
    run_test_with_chunk_size(size, iterations, NumaMode::Auto, None, 64 * 1024 * 1024);
    println!();
    
    println!("=== Summary ===");
    println!("Test 1 (Disabled): No thread pinning - baseline");
    println!("Test 2 (Auto):     Pin threads only on multi-socket");
    println!("Test 3 (Force):    Always pin threads - best for NUMA");
    println!("Test 4 (Physical): Physical cores only, no hyperthreading");
    println!("Test 5 (64 MB):    Optimal buffer size - expect ~2x improvement");
}

fn run_test_with_chunk_size(size: usize, iterations: usize, numa_mode: NumaMode, max_threads: Option<usize>, chunk_size: usize) {
    let config = GeneratorConfig {
        size,
        dedup_factor: 1,
        compress_factor: 1,
        block_size: None,
        seed: None,
        numa_mode,
        max_threads,
        numa_node: None,
    };
    
    let mut durations = Vec::new();
    
    for i in 1..=iterations {
        let start = Instant::now();
        
        // Use streaming mode (DataGenerator) like the original benchmark
        let mut generator = DataGenerator::new(config.clone());
        let mut buffer = vec![0u8; chunk_size];
        let mut total_generated = 0;
        
        while total_generated < size {
            let bytes_written = generator.fill_chunk(&mut buffer);
            if bytes_written == 0 {
                break;
            }
            total_generated += bytes_written;
        }
        
        let duration = start.elapsed();
        durations.push(duration);
        
        let secs = duration.as_secs_f64();
        let gb = size as f64 / (1024.0 * 1024.0 * 1024.0);
        let throughput = gb / secs;
        
        let chunk_mb = chunk_size / (1024 * 1024);
        println!("  Iteration {} ({} MB chunks): {:.2} GB/s ({:.3}s)", i, chunk_mb, throughput, secs);
    }
    
    // Calculate average
    let avg_secs: f64 = durations.iter().map(|d| d.as_secs_f64()).sum::<f64>() / iterations as f64;
    let gb = size as f64 / (1024.0 * 1024.0 * 1024.0);
    let avg_throughput = gb / avg_secs;
    
    let threads = max_threads.unwrap_or_else(num_cpus::get);
    let per_core = avg_throughput / threads as f64;
    
    println!("  Average: {:.2} GB/s ({} threads, {:.2} GB/s per thread)", 
             avg_throughput, threads, per_core);
}
