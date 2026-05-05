// Benchmark for streaming data generation throughput
// Tests different block sizes and methods to find optimal performance
//
// TWO BENCHMARK MODELS:
//
// 1. Streaming (single-threaded): measures one DataGenerator::fill_chunk() loop
//    with a single caller.  This is the model for a single large-object writer.
//    Generator and buffer created ONCE and reused across all iterations.
//
// 2. Concurrent (warp model): models N parallel object writers sharing ONE
//    UniqueXorStream singleton.  Each writer has its own output buffer and calls
//    xor_stream.fill() simultaneously.  This is how sai3-bench and warp work:
//    many goroutines/Tokio tasks, each writing a different object, each filling
//    its own buffer concurrently.  Aggregate GB/s is what matters here.
//
// No seed is ever set; normal operation uses entropy seeded at construction.
// Buffers and generators are never recreated during timing loops.

use dgen_data::generator::{DataGenerator, GenerationMethod, GeneratorConfig, NumaMode};
use dgen_data::UniqueXorStream;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

/// Bytes generated per timed iteration (streaming bench).
const ITER_SIZE: usize = 10 * 1024 * 1024 * 1024; // 10 GB per run

/// Number of timed iterations.
const ITERATIONS: usize = 5;

/// Warmup before timing starts.
const WARMUP_SIZE: usize = 1024 * 1024 * 1024; // 1 GB

/// Buffer the caller provides to fill_chunk (streaming bench).
const CHUNK_SIZE: usize = 32 * 1024 * 1024; // 32 MB

/// How long each concurrent-bench run lasts.
const CONCURRENT_SECS: u64 = 5;

/// Object size each concurrent worker fills per call (matches typical sai3-bench workload).
const OBJECT_SIZE: usize = 8 * 1024 * 1024; // 8 MB

/// DLIO benchmark object size — 315 KiB files as used in the MLPerf Storage benchmark.
const DLIO_OBJECT_SIZE: usize = 315 * 1024; // 315 KiB

/// Block sizes swept in the streaming benchmarks.
const STREAM_BLOCK_SIZES: &[usize] = &[
    256 * 1024,        //  256 KB
    1024 * 1024,       //    1 MB
    4 * 1024 * 1024,   //    4 MB
    16 * 1024 * 1024,  //   16 MB
];

// ── Streaming benchmark ───────────────────────────────────────────────────────

fn benchmark_streaming(method: GenerationMethod, block_size: usize) {
    let method_name = match method {
        GenerationMethod::Parallel => "Parallel",
        GenerationMethod::XorStream => "XorStream",
    };
    println!("\n{}", "=".repeat(80));
    println!(
        "STREAMING  Method={method_name}  block_size={} MB  chunk={} MB",
        block_size / (1024 * 1024),
        CHUNK_SIZE / (1024 * 1024),
    );
    println!("{}", "=".repeat(80));

    let total_needed = WARMUP_SIZE + ITER_SIZE * ITERATIONS;

    let config = GeneratorConfig {
        size: total_needed,
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: NumaMode::Disabled,
        max_threads: None,
        numa_node: None,
        block_size: Some(block_size),
        seed: None,
        method,
    };
    let mut gen = DataGenerator::new(config);
    let mut buffer = vec![0u8; CHUNK_SIZE];

    // Warmup
    let mut warmup_done = 0;
    while warmup_done < WARMUP_SIZE {
        let n = gen.fill_chunk(&mut buffer);
        if n == 0 { break; }
        warmup_done += n;
    }
    println!("Warmup: {} MB done", warmup_done / (1024 * 1024));

    let mut run_gbps = Vec::with_capacity(ITERATIONS);
    for i in 1..=ITERATIONS {
        let mut bytes_done = 0usize;
        let start = Instant::now();
        while bytes_done < ITER_SIZE {
            let n = gen.fill_chunk(&mut buffer);
            if n == 0 { break; }
            bytes_done += n;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let gbps = (bytes_done as f64 / 1e9) / elapsed;
        run_gbps.push(gbps);
        println!(
            "  Run {:02}: {} GB in {:.3} s = {:.2} GB/s",
            i, bytes_done / (1024 * 1024 * 1024), elapsed, gbps,
        );
    }
    let avg = run_gbps.iter().sum::<f64>() / run_gbps.len() as f64;
    let min = run_gbps.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = run_gbps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("  avg={avg:.2} GB/s  min={min:.2}  max={max:.2}");
}

// ── Concurrent benchmark (warp model) ────────────────────────────────────────
//
// One shared UniqueXorStream singleton, N threads each filling their own
// OBJECT_SIZE buffer in a tight loop for CONCURRENT_SECS seconds.
// Aggregate bytes / elapsed → total GB/s across all concurrent workers.

fn benchmark_concurrent(num_threads: usize) {
    println!("\n{}", "=".repeat(80));
    println!(
        "CONCURRENT  threads={}  object_size={} MB  duration={}s",
        num_threads,
        OBJECT_SIZE / (1024 * 1024),
        CONCURRENT_SECS,
    );
    println!("(warp model: shared singleton, each thread fills its own buffer)");
    println!("{}", "=".repeat(80));

    // Shared singleton — same as sai3-bench's OnceLock<UniqueXorStream>.
    let stream = Arc::new(UniqueXorStream::new());

    // Warmup: 1 full second before timing.
    let warmup_deadline = Instant::now() + Duration::from_secs(1);
    {
        let stream = Arc::clone(&stream);
        let mut buf = vec![0u8; OBJECT_SIZE];
        while Instant::now() < warmup_deadline {
            stream.fill(&mut buf);
        }
    }

    let barrier = Arc::new(Barrier::new(num_threads + 1)); // +1 for main

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let stream = Arc::clone(&stream);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; OBJECT_SIZE];
                // Each thread allocates its buffer ONCE, reuses it every fill.
                barrier.wait(); // all threads start simultaneously
                let deadline = Instant::now() + Duration::from_secs(CONCURRENT_SECS);
                let mut bytes = 0u64;
                while Instant::now() < deadline {
                    stream.fill(&mut buf);
                    bytes += OBJECT_SIZE as u64;
                }
                bytes
            })
        })
        .collect();

    barrier.wait(); // release all threads at once
    let t_start = Instant::now();

    let total_bytes: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let elapsed = t_start.elapsed().as_secs_f64();

    let total_gbps = (total_bytes as f64 / 1e9) / elapsed;
    let per_thread = total_gbps / num_threads as f64;
    println!(
        "  Total: {:.1} GB in {:.2} s = {:.2} GB/s  ({:.2} GB/s per thread)",
        total_bytes as f64 / 1e9, elapsed, total_gbps, per_thread,
    );
}

// ── Concurrent benchmark (Parallel method) ───────────────────────────────────
//
// N threads each with their own DataGenerator (GenerationMethod::Parallel).
// gen_threads=1  → each generator runs single-threaded sequential Xoshiro256++
//                  (fair per-core comparison with XorStream)
// gen_threads=N  → each generator creates an N-thread Rayon pool
//                  (N callers × N threads = N² OS threads = oversubscription)
//
// DataGenerator::new() with Parallel creates its own dedicated Rayon pool,
// so N concurrent generators with full thread counts = N×ncpus OS threads on
// ncpus CPUs — this is the critical difference from XorStream which is lock-free.

fn benchmark_concurrent_parallel(num_threads: usize, gen_threads: usize) {
    let thread_label = if gen_threads == 1 {
        "1 thread/gen — sequential Xoshiro256++".to_string()
    } else {
        format!(
            "{} threads/gen — dedicated Rayon pool ({} total OS threads)",
            gen_threads,
            num_threads * gen_threads
        )
    };
    println!("\n{}", "=".repeat(80));
    println!(
        "CONCURRENT PARALLEL  callers={}  gen_threads={}  object={}MB  dur={}s",
        num_threads,
        gen_threads,
        OBJECT_SIZE / (1024 * 1024),
        CONCURRENT_SECS,
    );
    println!("  ({})", thread_label);
    println!("{}", "=".repeat(80));

    // Size per generator: large enough never to exhaust in CONCURRENT_SECS seconds.
    // 200 GB → 51,200 blocks of 4 MB → copy_lens vec of ~400 KB each. Fine.
    let gen_size: usize = 200 * 1024 * 1024 * 1024;

    let barrier = Arc::new(Barrier::new(num_threads + 1));

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let config = GeneratorConfig {
                    size: gen_size,
                    dedup_factor: 1,
                    compress_factor: 1,
                    numa_mode: NumaMode::Disabled,
                    max_threads: Some(gen_threads),
                    numa_node: None,
                    block_size: None, // default 4 MB
                    seed: None,
                    method: GenerationMethod::Parallel,
                };
                let mut gen = DataGenerator::new(config);
                let mut buf = vec![0u8; OBJECT_SIZE];
                barrier.wait(); // synchronized start
                let deadline = Instant::now() + Duration::from_secs(CONCURRENT_SECS);
                let mut bytes = 0u64;
                while Instant::now() < deadline {
                    let n = gen.fill_chunk(&mut buf);
                    if n == 0 { break; } // should never happen given gen_size
                    bytes += n as u64;
                }
                bytes
            })
        })
        .collect();

    barrier.wait(); // release all threads at once
    let t_start = Instant::now();

    let total_bytes: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let elapsed = t_start.elapsed().as_secs_f64();

    let total_gbps = (total_bytes as f64 / 1e9) / elapsed;
    let per_thread = total_gbps / num_threads as f64;
    println!(
        "  Total: {:.1} GB in {:.2} s = {:.2} GB/s  ({:.2} GB/s per caller)",
        total_bytes as f64 / 1e9,
        elapsed,
        total_gbps,
        per_thread,
    );
}

// ── DLIO benchmark helpers ────────────────────────────────────────────────────
//
// Models the MLPerf Storage / DLIO workload:
//   - num_procs × workers_per_proc concurrent callers, each filling DLIO_OBJECT_SIZE buffers
//   - Simulates P Python processes (separate address spaces) with W DataLoader workers each
//
// Three variants:
//   XorStream       — one UniqueXorStream per simulated process, W workers share it (lock-free)
//   Parallel reused — one DataGenerator per worker, reused across calls; max_threads=1
//   Parallel fresh  — new DataGenerator every call (current Python generate_buffer() behavior)
//
// All variants: barrier-synchronized start, CONCURRENT_SECS timed run, aggregate GB/s.

fn dlio_header(label: &str, procs: usize, workers: usize) {
    println!("\n{}", "=".repeat(80));
    println!(
        "DLIO  {}  procs={}  workers/proc={}  obj={} KiB  dur={}s",
        label,
        procs,
        workers,
        DLIO_OBJECT_SIZE / 1024,
        CONCURRENT_SECS,
    );
    println!("  total callers={}", procs * workers);
    println!("{}", "=".repeat(80));
}

fn dlio_print_result(total_bytes: u64, elapsed: f64, total_callers: usize) {
    let total_gbps = (total_bytes as f64 / 1e9) / elapsed;
    let calls = total_bytes / DLIO_OBJECT_SIZE as u64;
    let per_caller = total_gbps / total_callers as f64;
    println!(
        "  Total: {:.2} GB in {:.2} s = {:.3} GB/s  ({:.4} GB/s per caller, ~{} obj/s)",
        total_bytes as f64 / 1e9,
        elapsed,
        total_gbps,
        per_caller,
        calls as u64 / elapsed as u64,
    );
}

/// XorStream variant — one singleton per simulated process, workers share it.
fn benchmark_dlio_xor(num_procs: usize, workers_per_proc: usize) {
    dlio_header("XorStream (1 singleton/proc, workers share)", num_procs, workers_per_proc);

    let total_workers = num_procs * workers_per_proc;
    let barrier = Arc::new(Barrier::new(total_workers + 1));

    // One Arc<UniqueXorStream> per simulated process — each is completely independent.
    let singletons: Vec<Arc<UniqueXorStream>> = (0..num_procs)
        .map(|_| Arc::new(UniqueXorStream::new()))
        .collect();

    let handles: Vec<_> = singletons
        .iter()
        .flat_map(|stream| {
            (0..workers_per_proc).map(|_| {
                let stream = Arc::clone(stream);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; DLIO_OBJECT_SIZE];
                    barrier.wait();
                    let deadline = Instant::now() + Duration::from_secs(CONCURRENT_SECS);
                    let mut bytes = 0u64;
                    while Instant::now() < deadline {
                        stream.fill(&mut buf);
                        bytes += DLIO_OBJECT_SIZE as u64;
                    }
                    bytes
                })
            })
        })
        .collect();

    barrier.wait();
    let t_start = Instant::now();
    let total_bytes: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    dlio_print_result(total_bytes, t_start.elapsed().as_secs_f64(), total_workers);
}

/// Parallel-reused variant — one DataGenerator per worker, created once and reused.
/// max_threads=1 because N processes already saturate all cores; Rayon pool per worker
/// would cause oversubscription.
fn benchmark_dlio_parallel_reused(num_procs: usize, workers_per_proc: usize) {
    let total_workers = num_procs * workers_per_proc;
    dlio_header(
        "Parallel reused (1 DataGenerator/worker, max_threads=1)",
        num_procs,
        workers_per_proc,
    );

    // Large enough not to exhaust during the timed run (320 GB >> 5s × any realistic throughput).
    let gen_size: usize = 320 * 1024 * 1024 * 1024;
    let barrier = Arc::new(Barrier::new(total_workers + 1));

    let handles: Vec<_> = (0..total_workers)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let config = GeneratorConfig {
                    size: gen_size,
                    dedup_factor: 1,
                    compress_factor: 1,
                    numa_mode: NumaMode::Disabled,
                    max_threads: Some(1), // no Rayon pool — single-threaded Xoshiro256++
                    numa_node: None,
                    block_size: Some(DLIO_OBJECT_SIZE.max(1024 * 1024)), // ≥1 MB
                    seed: None,
                    method: GenerationMethod::Parallel,
                };
                let mut gen = DataGenerator::new(config);
                let mut buf = vec![0u8; DLIO_OBJECT_SIZE];
                barrier.wait();
                let deadline = Instant::now() + Duration::from_secs(CONCURRENT_SECS);
                let mut bytes = 0u64;
                while Instant::now() < deadline {
                    let n = gen.fill_chunk(&mut buf);
                    if n == 0 { break; }
                    bytes += n as u64;
                }
                bytes
            })
        })
        .collect();

    barrier.wait();
    let t_start = Instant::now();
    let total_bytes: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    dlio_print_result(total_bytes, t_start.elapsed().as_secs_f64(), total_workers);
}

/// Parallel-fresh variant — new DataGenerator constructed on every single call.
/// This is what the current Python API does: `generate_buffer(size)` calls
/// `generate_data(config)` which creates a fresh DataGenerator internally each time.
/// Shows the true per-call overhead of Parallel method at 315 KiB object size.
fn benchmark_dlio_parallel_fresh(num_procs: usize, workers_per_proc: usize) {
    let total_workers = num_procs * workers_per_proc;
    dlio_header(
        "Parallel fresh  (new DataGenerator per call — current Python API)",
        num_procs,
        workers_per_proc,
    );

    let barrier = Arc::new(Barrier::new(total_workers + 1));

    let handles: Vec<_> = (0..total_workers)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; DLIO_OBJECT_SIZE];
                barrier.wait();
                let deadline = Instant::now() + Duration::from_secs(CONCURRENT_SECS);
                let mut bytes = 0u64;
                while Instant::now() < deadline {
                    // Recreate DataGenerator every call — this is what generate_buffer() does.
                    let config = GeneratorConfig {
                        size: DLIO_OBJECT_SIZE,
                        dedup_factor: 1,
                        compress_factor: 1,
                        numa_mode: NumaMode::Disabled,
                        max_threads: Some(1),
                        numa_node: None,
                        block_size: Some(DLIO_OBJECT_SIZE.max(1024 * 1024)),
                        seed: None,
                        method: GenerationMethod::Parallel,
                    };
                    let mut gen = DataGenerator::new(config);
                    let n = gen.fill_chunk(&mut buf);
                    if n == 0 { break; }
                    bytes += n as u64;
                }
                bytes
            })
        })
        .collect();

    barrier.wait();
    let t_start = Instant::now();
    let total_bytes: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    dlio_print_result(total_bytes, t_start.elapsed().as_secs_f64(), total_workers);
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // Optional CLI filters — pass one or more section names to run only those.
    // Examples:
    //   cargo bench --bench streaming-bench                          (run all)
    //   cargo bench --bench streaming-bench -- streaming             (streaming only)
    //   cargo bench --bench streaming-bench -- parallel              (Parallel streaming + concurrent)
    //   cargo bench --bench streaming-bench -- xor                   (XorStream streaming + concurrent)
    //   cargo bench --bench streaming-bench -- concurrent            (all concurrent tests)
    //   cargo bench --bench streaming-bench -- par-concurrent        (Parallel concurrent only)
    //   cargo bench --bench streaming-bench -- oversubscribed        (oversubscription demo)
    //   cargo bench --bench streaming-bench -- dlio                  (DLIO 315 KiB scenario)
    //   cargo bench --bench streaming-bench -- concurrent dlio       (concurrent + dlio only)
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run_all = args.is_empty();
    let want = |name: &str| -> bool { run_all || args.iter().any(|a| a == name) };

    let ncpus = num_cpus::get();
    println!("RUST STREAMING + CONCURRENT THROUGHPUT BENCHMARK");
    println!(
        "system: {} logical CPUs  |  iter_size={} GB  chunk={} MB  concurrent_dur={}s",
        ncpus,
        ITER_SIZE / (1024 * 1024 * 1024),
        CHUNK_SIZE / (1024 * 1024),
        CONCURRENT_SECS,
    );
    if !run_all {
        println!("Running sections: {}", args.join(", "));
    }

    // ── Streaming: Parallel method, multiple block sizes ──────────────────────
    // ONE caller; DataGenerator uses all ncpus cores internally via Rayon.
    if want("streaming") || want("parallel") {
        println!("\n\n{}", "#".repeat(80));
        println!(
            "# STREAMING Parallel — 1 caller, all {ncpus} cores via Rayon (block-size sweep)"
        );
        println!("{}", "#".repeat(80));
        for &bs in STREAM_BLOCK_SIZES {
            benchmark_streaming(GenerationMethod::Parallel, bs);
        }
    }

    // ── Streaming: XorStream method, multiple block sizes ─────────────────────
    // ONE caller, single-core XOR keystream.  block_size has minimal effect here
    // (XorStream works at 16 KiB granularity internally), but we sweep to confirm.
    if want("streaming") || want("xor") || want("xorstream") {
        println!("\n\n{}", "#".repeat(80));
        println!("# STREAMING XorStream — 1 caller, single-core XOR keystream (block-size sweep)");
        println!("{}", "#".repeat(80));
        for &bs in STREAM_BLOCK_SIZES {
            benchmark_streaming(GenerationMethod::XorStream, bs);
        }
    }

    // ── Concurrent: XorStream (warp model) ────────────────────────────────────
    // Shared singleton, N parallel workers, lock-free via AtomicU64 offset.
    // Each worker uses exactly 1 CPU core — aggregate GB/s scales with threads.
    if want("concurrent") || want("xor-concurrent") || want("xor") || want("xorstream") {
        println!("\n\n{}", "#".repeat(80));
        println!(
            "# CONCURRENT XorStream — shared singleton, N workers, lock-free, 1 core/worker"
        );
        println!("{}", "#".repeat(80));
        for &t in &[1usize, 4, 8, 16] {
            if t <= ncpus {
                benchmark_concurrent(t);
            }
        }
        if ncpus > 16 {
            benchmark_concurrent(ncpus);
        }
    }

    // ── Concurrent: Parallel, 1 thread per DataGenerator ──────────────────────
    // Fair per-core comparison: each generator does single-threaded Xoshiro256++
    // (no Rayon pool — fill_chunk falls back to sequential when max_threads=1).
    if want("concurrent") || want("par-concurrent") || want("parallel") {
        println!("\n\n{}", "#".repeat(80));
        println!("# CONCURRENT Parallel (1 thread/gen) — N DataGenerators, sequential Xoshiro++");
        println!("# Fair per-core comparison: same 1-core-per-worker model as XorStream");
        println!("{}", "#".repeat(80));
        for &t in &[1usize, 4, 8, 16] {
            if t <= ncpus {
                benchmark_concurrent_parallel(t, 1);
            }
        }
        if ncpus > 16 {
            benchmark_concurrent_parallel(ncpus, 1);
        }
    }

    // ── Concurrent: Parallel, full threads per generator (oversubscribed) ─────
    // Real-world failure mode: each of N concurrent tasks naïvely creates a
    // DataGenerator with max_threads=None (default = all ncpus cores).
    // Result: N × ncpus OS threads contending for ncpus CPUs.
    if want("concurrent") || want("oversubscribed") || want("par-oversubscribed") {
        println!("\n\n{}", "#".repeat(80));
        println!(
            "# CONCURRENT Parallel (oversubscribed) — N generators × {ncpus} threads = {} total",
            ncpus * ncpus
        );
        println!("# Shows Rayon pool contention when N callers each spawn full thread pools");
        println!("{}", "#".repeat(80));
        for &t in &[1usize, 4, 8, 16] {
            if t <= ncpus {
                benchmark_concurrent_parallel(t, ncpus);
            }
        }
        if ncpus > 16 {
            benchmark_concurrent_parallel(ncpus, ncpus);
        }
    }

    // ── DLIO: 315 KiB objects, 8 processes × variable workers ─────────────────
    // Models the MLPerf Storage UNet3D/ResNet50 workload running on 8 Python processes.
    // Each Python process is simulated as an independent group of OS threads with its
    // own XorStream singleton (or DataGenerator per worker for Parallel variants).
    //
    // Process counts tested: 1, 2, 4, 8 (at workers_per_proc=1 and =4).
    // XorStream vs Parallel-reused vs Parallel-fresh (current Python API).
    if want("dlio") {
        let ncpus_local = num_cpus::get();
        println!("\n\n{}", "#".repeat(80));
        println!("# DLIO 315 KiB — MLPerf Storage workload simulation");
        println!("# Comparing XorStream vs Parallel for small-object concurrent generation");
        println!("# object_size={} KiB  duration={}s", DLIO_OBJECT_SIZE / 1024, CONCURRENT_SECS);
        println!("{}", "#".repeat(80));

        // -- 1 worker per process (serial DataLoader) --------------------------
        println!("\n--- 1 worker per process (serial DataLoader) ---");
        for &procs in &[1usize, 2, 4, 8] {
            if procs * 1 <= ncpus_local * 2 {
                benchmark_dlio_xor(procs, 1);
                benchmark_dlio_parallel_reused(procs, 1);
                benchmark_dlio_parallel_fresh(procs, 1);
            }
        }

        // -- 4 workers per process (typical DLIO num_subfolders/num_workers) ---
        println!("\n--- 4 workers per process (typical DLIO num_workers=4) ---");
        for &procs in &[1usize, 2, 4, 8] {
            if procs * 4 <= ncpus_local * 2 {
                benchmark_dlio_xor(procs, 4);
                benchmark_dlio_parallel_reused(procs, 4);
                benchmark_dlio_parallel_fresh(procs, 4);
            }
        }
    }

    println!("\n{}", "=".repeat(80));
    println!("BENCHMARK COMPLETE");
    println!("{}", "=".repeat(80));
}
