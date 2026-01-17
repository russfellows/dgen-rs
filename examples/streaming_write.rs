// Generate streaming data and write to file - stays in Rust (no Python GIL overhead)
// This will be exposed to Python via PyO3

use dgen_rs::{DataGenerator, GeneratorConfig, NumaMode};
use std::fs::File;
use std::io::Write;
use std::time::Instant;

fn main() -> std::io::Result<()> {
    let total_size = 100 * 1024 * 1024 * 1024;  // 100 GB
    let chunk_size = 64 * 1024 * 1024;  // 64 MB
    
    let config = GeneratorConfig {
        size: total_size,
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: NumaMode::Auto,
        max_threads: None,
    };
    
    let mut gen = DataGenerator::new(config);
    let mut buffer = vec![0u8; chunk_size];
    
    // Open file for writing
    let mut file = File::create("test_output.bin")?;
    
    let start = Instant::now();
    let mut total_written = 0;
    
    while !gen.is_complete() {
        let nbytes = gen.fill_chunk(&mut buffer);
        if nbytes == 0 {
            break;
        }
        
        // Write to file
        file.write_all(&buffer[..nbytes])?;
        total_written += nbytes;
    }
    
    file.sync_all()?;
    let elapsed = start.elapsed().as_secs_f64();
    
    println!("Generated and wrote {} GB in {:.2}s", total_written / (1024*1024*1024), elapsed);
    println!("Throughput: {:.2} GB/s", (total_written as f64) / elapsed / 1e9);
    
    Ok(())
}
