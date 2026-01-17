// src/generator.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! High-performance data generation with controllable deduplication and compression
//!
//! Ported from s3dlio/src/data_gen_alt.rs with NUMA optimizations

use rand::{Rng, RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use rayon::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::*;

#[cfg(feature = "numa")]
use crate::numa::NumaTopology;

/// NUMA optimization mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumaMode {
    /// Auto-detect: enable NUMA optimizations only on multi-node systems
    #[default]
    Auto,
    /// Force NUMA: enable optimizations even on UMA systems (for testing)
    Force,
    /// Disable: never use NUMA optimizations (default for cloud/VM)
    Disabled,
}

/// Configuration for data generation
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// Total size in bytes
    pub size: usize,
    /// Deduplication factor (1 = no dedup, N = N:1 logical:physical ratio)
    pub dedup_factor: usize,
    /// Compression factor (1 = incompressible, N = N:1 logical:physical ratio)
    pub compress_factor: usize,
    /// NUMA optimization mode (Auto, Force, or Disabled)
    pub numa_mode: NumaMode,
    /// Maximum number of threads to use (None = use all available cores)
    pub max_threads: Option<usize>,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            size: BLOCK_SIZE,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None, // Use all available cores
        }
    }
}

/// Simple API: Generate data with default config
///
/// # Parameters
/// - `size`: Total bytes to generate
/// - `dedup`: Deduplication factor (1 = no dedup, N = N:1 ratio)
/// - `compress`: Compression factor (1 = incompressible, N = N:1 ratio)
///
/// # Example
/// ```rust
/// use dgen_rs::generate_data_simple;
///
/// // Generate 100 MiB incompressible data with no deduplication
/// let data = generate_data_simple(100 * 1024 * 1024, 1, 1);
/// assert_eq!(data.len(), 100 * 1024 * 1024);
/// ```
pub fn generate_data_simple(size: usize, dedup: usize, compress: usize) -> Vec<u8> {
    let config = GeneratorConfig {
        size,
        dedup_factor: dedup.max(1),
        compress_factor: compress.max(1),
        numa_mode: NumaMode::Auto,
        max_threads: None,
    };
    generate_data(config)
}

/// Generate data with full configuration
///
/// # Algorithm
/// 1. Fill blocks with Xoshiro256++ keystream (high entropy baseline)
/// 2. Add local back-references for compression
/// 3. Use round-robin deduplication across unique blocks
/// 4. Parallel generation via rayon (NUMA-aware if enabled)
///
/// # Performance
/// - 5-15 GB/s per core with incompressible data
/// - 1-4 GB/s with compression enabled (depends on compress factor)
/// - Near-linear scaling with CPU cores
pub fn generate_data(config: GeneratorConfig) -> Vec<u8> {
    tracing::info!(
        "Starting data generation: size={}, dedup={}, compress={}",
        config.size,
        config.dedup_factor,
        config.compress_factor
    );

    let size = config.size.max(MIN_SIZE);
    let nblocks = size.div_ceil(BLOCK_SIZE);

    let dedup_factor = config.dedup_factor.max(1);
    let unique_blocks = if dedup_factor > 1 {
        ((nblocks as f64) / (dedup_factor as f64)).round().max(1.0) as usize
    } else {
        nblocks
    };

    tracing::debug!(
        "Generating: size={}, blocks={}, dedup={}, unique_blocks={}, compress={}",
        size,
        nblocks,
        dedup_factor,
        unique_blocks,
        config.compress_factor
    );

    // Calculate per-block copy lengths using integer error accumulation
    // This ensures even distribution of compression across blocks
    let (f_num, f_den) = if config.compress_factor > 1 {
        (config.compress_factor - 1, config.compress_factor)
    } else {
        (0, 1)
    };
    let floor_len = (f_num * BLOCK_SIZE) / f_den;
    let rem = (f_num * BLOCK_SIZE) % f_den;

    let copy_lens: Vec<usize> = {
        let mut v = Vec::with_capacity(unique_blocks);
        let mut err = 0;
        for _ in 0..unique_blocks {
            err += rem;
            if err >= f_den {
                err -= f_den;
                v.push(floor_len + 1);
            } else {
                v.push(floor_len);
            }
        }
        v
    };

    // Per-call entropy for RNG seeding
    let call_entropy = generate_call_entropy();

    // Allocate buffer
    let total_size = nblocks * BLOCK_SIZE;
    tracing::debug!("Allocating {} bytes ({} blocks)", total_size, nblocks);
    let mut data: Vec<u8> = vec![0u8; total_size];

    // Configure thread pool based on config
    let num_threads = config.max_threads.unwrap_or_else(num_cpus::get);
    tracing::info!("Using {} threads for parallel generation", num_threads);

    // NUMA optimization check
    #[cfg(feature = "numa")]
    let numa_topology = if config.numa_mode != NumaMode::Disabled {
        NumaTopology::detect().ok()
    } else {
        None
    };

    #[cfg(feature = "numa")]
    let should_optimize_numa = if let Some(ref topology) = numa_topology {
        let optimize = match config.numa_mode {
            NumaMode::Auto => topology.num_nodes > 1,
            NumaMode::Force => true,
            NumaMode::Disabled => false,
        };

        if optimize {
            tracing::info!(
                "NUMA optimization enabled: {} nodes detected",
                topology.num_nodes
            );
        } else {
            tracing::debug!(
                "NUMA optimization not needed: {} nodes detected",
                topology.num_nodes
            );
        }
        optimize
    } else {
        false
    };

    #[cfg(not(feature = "numa"))]
    let should_optimize_numa = false;

    tracing::debug!("Starting parallel generation with rayon");

    // Build thread pool with optional NUMA-aware thread pinning
    // Only pin threads on true NUMA systems (>1 node) - adds overhead on UMA
    #[cfg(all(feature = "numa", feature = "thread-pinning"))]
    let pool = if should_optimize_numa {
        if let Some(ref topology) = numa_topology {
            if topology.num_nodes > 1 {
                tracing::debug!(
                    "Configuring NUMA-aware thread pinning for {} nodes",
                    topology.num_nodes
                );

                // Build CPU affinity mapping (wrap in Arc for sharing across threads)
                let cpu_map = std::sync::Arc::new(build_cpu_affinity_map(topology, num_threads));

                rayon::ThreadPoolBuilder::new()
                    .num_threads(num_threads)
                    .spawn_handler(move |thread| {
                        let cpu_map = cpu_map.clone();
                        let mut b = std::thread::Builder::new();
                        if let Some(name) = thread.name() {
                            b = b.name(name.to_owned());
                        }
                        if let Some(stack_size) = thread.stack_size() {
                            b = b.stack_size(stack_size);
                        }

                        b.spawn(move || {
                            // Pin this thread to specific CPU cores
                            let thread_id = rayon::current_thread_index().unwrap_or(0);
                            if let Some(core_ids) = cpu_map.get(&thread_id) {
                                pin_thread_to_cores(core_ids);
                            }
                            thread.run()
                        })?;
                        Ok(())
                    })
                    .build()
                    .expect("Failed to create NUMA-aware thread pool")
            } else {
                tracing::debug!("Skipping thread pinning on UMA system (would add overhead)");
                rayon::ThreadPoolBuilder::new()
                    .num_threads(num_threads)
                    .build()
                    .expect("Failed to create thread pool")
            }
        } else {
            rayon::ThreadPoolBuilder::new()
                .num_threads(num_threads)
                .build()
                .expect("Failed to create thread pool")
        }
    } else {
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .expect("Failed to create thread pool")
    };

    #[cfg(not(all(feature = "numa", feature = "thread-pinning")))]
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()
        .expect("Failed to create thread pool");

    // First-touch memory initialization for NUMA locality
    // Only beneficial on true NUMA systems (>1 node)
    // On UMA systems, this just adds overhead
    #[cfg(feature = "numa")]
    if should_optimize_numa {
        if let Some(ref topology) = numa_topology {
            if topology.num_nodes > 1 {
                tracing::debug!(
                    "Performing first-touch memory initialization for {} NUMA nodes",
                    topology.num_nodes
                );
                pool.install(|| {
                    data.par_chunks_mut(BLOCK_SIZE).for_each(|chunk| {
                        // Touch each page to allocate it locally
                        // Linux allocates memory on the node of the thread that first writes to it
                        chunk[0] = 0;
                        if chunk.len() > 4096 {
                            chunk[chunk.len() - 1] = 0;
                        }
                    });
                });
            } else {
                tracing::trace!("Skipping first-touch on UMA system");
            }
        }
    }

    pool.install(|| {
        data.par_chunks_mut(BLOCK_SIZE)
            .enumerate()
            .for_each(|(i, chunk)| {
                let ub = i % unique_blocks;
                tracing::trace!("Filling block {} (unique block {})", i, ub);
                fill_block(chunk, ub, copy_lens[ub].min(chunk.len()), call_entropy);
            });
    });

    tracing::debug!("Parallel generation complete, truncating to {} bytes", size);
    // Truncate to requested size
    data.truncate(size);
    data
}

/// Fill a single block with controlled compression
///
/// # Algorithm
/// 1. Fill entire block with Xoshiro256++ keystream
/// 2. Add local back-references to achieve target compressibility
///
/// # Parameters
/// - `out`: Output buffer (BLOCK_SIZE bytes)
/// - `unique_block_idx`: Index of unique block (for RNG seeding)
/// - `copy_len`: Target bytes to make compressible
/// - `call_entropy`: Per-call RNG seed
fn fill_block(out: &mut [u8], unique_block_idx: usize, copy_len: usize, call_entropy: u64) {
    tracing::trace!(
        "fill_block: idx={}, copy_len={}, out_len={}",
        unique_block_idx,
        copy_len,
        out.len()
    );

    // Seed RNG uniquely per block
    let seed = call_entropy ^ ((unique_block_idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);

    // Step 1: Fill with high-entropy keystream
    tracing::trace!("Filling {} bytes with RNG keystream", out.len());
    rng.fill_bytes(out);

    // Step 2: Add local back-references for compression
    if copy_len == 0 || out.len() <= 1 {
        return;
    }

    let mut made = 0usize;
    while made < copy_len {
        let remaining = copy_len.saturating_sub(made);
        if remaining == 0 {
            break;
        }

        // Choose run length: 64-256 bytes
        let run_len = rng
            .random_range(MIN_RUN_LENGTH..=MAX_RUN_LENGTH)
            .min(remaining)
            .min(out.len() - 1);
        if run_len == 0 {
            break;
        }

        // Choose destination position
        if out.len() <= run_len {
            break;
        }
        let dst = rng.random_range(0..(out.len() - run_len));

        // Choose source position (back-reference)
        let max_back = dst.clamp(1, MAX_BACK_REF_DISTANCE);
        let back = rng.random_range(1..=max_back);
        let src = dst.saturating_sub(back);

        // Safety check
        if src + run_len > out.len() {
            break;
        }

        // Copy within block (handles overlaps)
        out.copy_within(src..(src + run_len), dst);
        made += run_len;
    }
    tracing::trace!("fill_block complete: made {} compressible bytes", made);
}

/// Generate per-call entropy from time + urandom
fn generate_call_entropy() -> u64 {
    let time_entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    let urandom_entropy: u64 = {
        let mut rng = rand::rng();
        rng.next_u64()
    };

    time_entropy.wrapping_add(urandom_entropy)
}

#[cfg(all(feature = "numa", feature = "thread-pinning"))]
use std::collections::HashMap;

/// Build CPU affinity map for thread pinning
#[cfg(all(feature = "numa", feature = "thread-pinning"))]
fn build_cpu_affinity_map(
    topology: &crate::numa::NumaTopology,
    num_threads: usize,
) -> HashMap<usize, Vec<usize>> {
    let mut map = HashMap::new();

    // Distribute threads across NUMA nodes round-robin
    let mut thread_id = 0;
    let mut node_idx = 0;

    while thread_id < num_threads {
        if let Some(node) = topology.nodes.get(node_idx % topology.nodes.len()) {
            // Assign threads to cores within this NUMA node
            let cores_per_thread = (node.cpus.len() as f64 / num_threads as f64).ceil() as usize;
            let cores_per_thread = cores_per_thread.max(1);

            let start_cpu = (thread_id * cores_per_thread) % node.cpus.len();
            let end_cpu = ((thread_id + 1) * cores_per_thread).min(node.cpus.len());

            let core_ids: Vec<usize> = node.cpus[start_cpu..end_cpu].to_vec();

            if !core_ids.is_empty() {
                tracing::trace!(
                    "Thread {} -> NUMA node {} cores {:?}",
                    thread_id,
                    node.node_id,
                    &core_ids
                );
                map.insert(thread_id, core_ids);
            }
        }

        thread_id += 1;
        node_idx += 1;
    }

    map
}

/// Pin current thread to specific CPU cores
#[cfg(all(feature = "numa", feature = "thread-pinning"))]
fn pin_thread_to_cores(core_ids: &[usize]) {
    if let Some(&first_core) = core_ids.first() {
        if let Some(core_ids_all) = core_affinity::get_core_ids() {
            if first_core < core_ids_all.len() {
                let core_id = core_ids_all[first_core];
                if core_affinity::set_for_current(core_id) {
                    tracing::trace!("Pinned thread to core {}", first_core);
                } else {
                    tracing::debug!("Failed to pin thread to core {}", first_core);
                }
            }
        }
    }
}

// =============================================================================
// Streaming Generator
// =============================================================================

/// Streaming data generator (like ObjectGenAlt from s3dlio)
pub struct DataGenerator {
    total_size: usize,
    current_pos: usize,
    #[allow(dead_code)]
    dedup_factor: usize,
    #[allow(dead_code)]
    compress_factor: usize,
    unique_blocks: usize,
    copy_lens: Vec<usize>,
    call_entropy: u64,
    max_threads: usize,  // Thread count for parallel generation
    thread_pool: Option<rayon::ThreadPool>,  // Reused thread pool (created once)
}

impl DataGenerator {
    /// Create new streaming generator
    pub fn new(config: GeneratorConfig) -> Self {
        tracing::info!(
            "Creating DataGenerator: size={}, dedup={}, compress={}",
            config.size,
            config.dedup_factor,
            config.compress_factor
        );

        let total_size = config.size.max(MIN_SIZE);
        let nblocks = total_size.div_ceil(BLOCK_SIZE);

        let dedup_factor = config.dedup_factor.max(1);
        let unique_blocks = if dedup_factor > 1 {
            ((nblocks as f64) / (dedup_factor as f64)).round().max(1.0) as usize
        } else {
            nblocks
        };

        // Calculate copy lengths
        let (f_num, f_den) = if config.compress_factor > 1 {
            (config.compress_factor - 1, config.compress_factor)
        } else {
            (0, 1)
        };
        let floor_len = (f_num * BLOCK_SIZE) / f_den;
        let rem = (f_num * BLOCK_SIZE) % f_den;

        let copy_lens: Vec<usize> = {
            let mut v = Vec::with_capacity(unique_blocks);
            let mut err = 0;
            for _ in 0..unique_blocks {
                err += rem;
                if err >= f_den {
                    err -= f_den;
                    v.push(floor_len + 1);
                } else {
                    v.push(floor_len);
                }
            }
            v
        };

        let call_entropy = generate_call_entropy();

        let max_threads = config.max_threads.unwrap_or_else(num_cpus::get);
        
        // Create thread pool ONCE for reuse (major performance optimization)
        let thread_pool = if max_threads > 1 {
            match rayon::ThreadPoolBuilder::new()
                .num_threads(max_threads)
                .build()
            {
                Ok(pool) => {
                    tracing::info!(
                        "DataGenerator configured with {} threads (thread pool created)",
                        max_threads
                    );
                    Some(pool)
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to create thread pool: {}, falling back to sequential",
                        e
                    );
                    None
                }
            }
        } else {
            tracing::info!("DataGenerator configured for single-threaded operation");
            None
        };

        Self {
            total_size,
            current_pos: 0,
            dedup_factor,
            compress_factor: config.compress_factor,
            unique_blocks,
            copy_lens,
            call_entropy,
            max_threads,
            thread_pool,
        }
    }

    /// Fill the next chunk of data
    ///
    /// Returns the number of bytes written. When this returns 0, generation is complete.
    /// 
    /// **Performance**: When buffer contains multiple blocks (>=8 MB), generation is parallelized
    /// using rayon. Small buffers (<8 MB) use sequential generation to avoid threading overhead.
    pub fn fill_chunk(&mut self, buf: &mut [u8]) -> usize {
        tracing::trace!(
            "fill_chunk called: pos={}/{}, buf_len={}",
            self.current_pos,
            self.total_size,
            buf.len()
        );

        if self.current_pos >= self.total_size {
            tracing::trace!("fill_chunk: already complete");
            return 0;
        }

        let remaining = self.total_size - self.current_pos;
        let to_write = buf.len().min(remaining);
        let chunk = &mut buf[..to_write];

        // Determine number of blocks to generate
        let start_block = self.current_pos / BLOCK_SIZE;
        let start_offset = self.current_pos % BLOCK_SIZE;
        let end_pos = self.current_pos + to_write;
        let end_block = (end_pos - 1) / BLOCK_SIZE;
        let num_blocks = end_block - start_block + 1;

        // Use parallel generation for large buffers (>=2 blocks), sequential for small
        // This avoids rayon overhead for tiny chunks
        const PARALLEL_THRESHOLD: usize = 2;
        
        if num_blocks >= PARALLEL_THRESHOLD && self.max_threads > 1 {
            // PARALLEL PATH: Generate all blocks in parallel
            self.fill_chunk_parallel(chunk, start_block, start_offset, num_blocks)
        } else {
            // SEQUENTIAL PATH: Generate blocks one at a time (small buffers or single-threaded)
            self.fill_chunk_sequential(chunk, start_block, start_offset, num_blocks)
        }
    }

    /// Sequential fill for small buffers
    #[inline]
    fn fill_chunk_sequential(
        &mut self,
        chunk: &mut [u8],
        start_block: usize,
        start_offset: usize,
        num_blocks: usize,
    ) -> usize {
        let mut offset = 0;

        for i in 0..num_blocks {
            let block_idx = start_block + i;
            let block_offset = if i == 0 { start_offset } else { 0 };
            let remaining_in_block = BLOCK_SIZE - block_offset;
            let to_copy = remaining_in_block.min(chunk.len() - offset);

            // Map to unique block
            let ub = block_idx % self.unique_blocks;

            // Generate full block
            let mut block_buf = vec![0u8; BLOCK_SIZE];
            fill_block(
                &mut block_buf,
                ub,
                self.copy_lens[ub].min(BLOCK_SIZE),
                self.call_entropy,
            );

            // Copy needed portion
            chunk[offset..offset + to_copy]
                .copy_from_slice(&block_buf[block_offset..block_offset + to_copy]);

            offset += to_copy;
        }

        let to_write = offset;
        self.current_pos += to_write;
        
        tracing::debug!(
            "fill_chunk_sequential: generated {} blocks ({} MiB) for {} byte chunk",
            num_blocks,
            num_blocks * 4,
            to_write
        );
        
        to_write
    }

    /// Parallel fill for large buffers (uses reused thread pool - ZERO COPY)
    fn fill_chunk_parallel(
        &mut self,
        chunk: &mut [u8],
        start_block: usize,
        start_offset: usize,
        num_blocks: usize,
    ) -> usize {
        use rayon::prelude::*;

        // Use stored thread pool if available, otherwise fall back to sequential
        let thread_pool = match &self.thread_pool {
            Some(pool) => pool,
            None => {
                // No thread pool - fall back to sequential
                return self.fill_chunk_sequential(chunk, start_block, start_offset, num_blocks);
            }
        };

        let call_entropy = self.call_entropy;
        let copy_lens = &self.copy_lens;
        let unique_blocks = self.unique_blocks;

        // ZERO-COPY: Generate directly into output buffer using par_chunks_mut
        // This is the same approach as generate_data() - no temporary allocations!
        thread_pool.install(|| {
            chunk
                .par_chunks_mut(BLOCK_SIZE)
                .enumerate()
                .for_each(|(i, block_chunk)| {
                    let block_idx = start_block + i;
                    let ub = block_idx % unique_blocks;
                    
                    // Handle first block with offset
                    if i == 0 && start_offset > 0 {
                        // Generate full block into temp, copy needed portion
                        let mut temp = vec![0u8; BLOCK_SIZE];
                        fill_block(&mut temp, ub, copy_lens[ub].min(BLOCK_SIZE), call_entropy);
                        let copy_len = BLOCK_SIZE.saturating_sub(start_offset).min(block_chunk.len());
                        block_chunk[..copy_len].copy_from_slice(&temp[start_offset..start_offset + copy_len]);
                    } else {
                        // Generate directly into output buffer (ZERO-COPY!)
                        let actual_len = block_chunk.len().min(BLOCK_SIZE);
                        fill_block(&mut block_chunk[..actual_len], ub, copy_lens[ub].min(actual_len), call_entropy);
                    }
                });
        });

        let to_write = chunk.len();
        self.current_pos += to_write;
        
        tracing::debug!(
            "fill_chunk_parallel: ZERO-COPY generated {} blocks ({} MiB) for {} byte chunk",
            num_blocks,
            num_blocks * 4,
            to_write
        );
        
        to_write
    }

    /// Reset generator to start
    pub fn reset(&mut self) {
        self.current_pos = 0;
    }

    /// Get current position
    pub fn position(&self) -> usize {
        self.current_pos
    }

    /// Get total size
    pub fn total_size(&self) -> usize {
        self.total_size
    }

    /// Check if generation is complete
    pub fn is_complete(&self) -> bool {
        self.current_pos >= self.total_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_tracing() {
        use tracing_subscriber::{fmt, EnvFilter};
        let _ = fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();
    }

    #[test]
    fn test_generate_minimal() {
        init_tracing();
        let data = generate_data_simple(100, 1, 1);
        assert_eq!(data.len(), BLOCK_SIZE);
    }

    #[test]
    fn test_generate_exact_block() {
        init_tracing();
        let data = generate_data_simple(BLOCK_SIZE, 1, 1);
        assert_eq!(data.len(), BLOCK_SIZE);
    }

    #[test]
    fn test_generate_multiple_blocks() {
        init_tracing();
        let size = BLOCK_SIZE * 10;
        let data = generate_data_simple(size, 1, 1);
        assert_eq!(data.len(), size);
    }

    #[test]
    fn test_streaming_generator() {
        init_tracing();
        eprintln!("Starting streaming generator test...");

        let config = GeneratorConfig {
            size: BLOCK_SIZE * 5,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None,
        };

        eprintln!("Config: {} blocks, {} bytes total", 5, BLOCK_SIZE * 5);

        let mut gen = DataGenerator::new(config.clone());
        let mut result = Vec::new();

        // Use a larger chunk size to avoid generating too many blocks
        // Generating 4 MiB block per 1024 bytes is 4096x overhead!
        let chunk_size = BLOCK_SIZE; // Use full block size for efficiency
        let mut chunk = vec![0u8; chunk_size];

        let mut iterations = 0;
        while !gen.is_complete() {
            let written = gen.fill_chunk(&mut chunk);
            if written == 0 {
                break;
            }
            result.extend_from_slice(&chunk[..written]);
            iterations += 1;

            if iterations % 10 == 0 {
                eprintln!(
                    "  Iteration {}: written={}, total={}",
                    iterations,
                    written,
                    result.len()
                );
            }
        }

        eprintln!(
            "Completed in {} iterations, generated {} bytes",
            iterations,
            result.len()
        );
        assert_eq!(result.len(), config.size);
        assert!(gen.is_complete());
    }
}
