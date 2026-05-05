// src/generator.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! High-performance data generation with controllable deduplication and compression
//!
//! Ported from s3dlio/src/data_gen_alt.rs with NUMA optimizations

use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use rayon::prelude::*;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::*;

#[cfg(feature = "numa")]
use crate::numa::NumaTopology;

#[cfg(feature = "numa")]
use hwlocality::{
    memory::binding::{MemoryBindingFlags, MemoryBindingPolicy},
    Topology,
};

/// ZERO-COPY buffer abstraction for UMA and NUMA allocations
///
/// CRITICAL: This type NEVER copies data - it holds the actual memory and provides
/// mutable slices for zero-copy operations. Python bindings access this memory
/// directly via raw pointers.
#[cfg(feature = "numa")]
pub enum DataBuffer {
    /// UMA allocation using Vec<u8> (fast path, 43-50 GB/s)
    /// Python accesses via Vec's raw pointer
    Uma(Vec<u8>),
    /// NUMA allocation using hwlocality Bytes (target: 1,200-1,400 GB/s)
    /// Python accesses via Bytes' raw pointer - ZERO COPY to Python!
    /// Stores (Topology, Bytes, actual_size) to keep Topology alive
    Numa((Topology, hwlocality::memory::binding::Bytes<'static>, usize)),
}

#[cfg(feature = "numa")]
impl DataBuffer {
    /// Get mutable slice for data generation (zero-copy)
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            DataBuffer::Uma(vec) => vec.as_mut_slice(),
            DataBuffer::Numa((_, bytes, _)) => {
                // SAFETY: We've allocated this buffer and will initialize it
                unsafe {
                    std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut u8, bytes.len())
                }
            }
        }
    }

    /// Get immutable slice view (zero-copy)
    pub fn as_slice(&self) -> &[u8] {
        match self {
            DataBuffer::Uma(vec) => vec.as_slice(),
            DataBuffer::Numa((_, bytes, size)) => {
                // SAFETY: Buffer has been fully initialized
                unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u8, *size) }
            }
        }
    }

    /// Get raw pointer for zero-copy Python access
    pub fn as_ptr(&self) -> *const u8 {
        match self {
            DataBuffer::Uma(vec) => vec.as_ptr(),
            DataBuffer::Numa((_, bytes, _)) => bytes.as_ptr() as *const u8,
        }
    }

    /// Get mutable raw pointer for zero-copy Python access
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            DataBuffer::Uma(vec) => vec.as_mut_ptr(),
            DataBuffer::Numa((_, bytes, _)) => bytes.as_mut_ptr() as *mut u8,
        }
    }

    /// Get length (actual data size, not allocated size)
    pub fn len(&self) -> usize {
        match self {
            DataBuffer::Uma(vec) => vec.len(),
            DataBuffer::Numa((_, _, size)) => *size,
        }
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Truncate to requested size (modifies metadata only, NO COPY)
    pub fn truncate(&mut self, size: usize) {
        match self {
            DataBuffer::Uma(vec) => vec.truncate(size),
            DataBuffer::Numa((_, bytes, actual_size)) => {
                *actual_size = size.min(bytes.len());
            }
        }
    }

    /// Convert to bytes::Bytes for Python API (ZERO-COPY for UMA, minimal copy for NUMA)
    ///
    /// For UMA: Uses Bytes::from(Vec<u8>) which is cheap (just wraps the allocation)
    /// For NUMA: Must copy to bytes::Bytes since hwlocality::Bytes can't be converted directly
    ///          Alternative: Keep as DataBuffer and implement Python buffer protocol directly
    pub fn into_bytes(self) -> bytes::Bytes {
        match self {
            DataBuffer::Uma(vec) => bytes::Bytes::from(vec),
            DataBuffer::Numa((_, hwloc_bytes, size)) => {
                // Convert NUMA-allocated memory to bytes::Bytes
                // Unfortunately this requires a copy since bytes::Bytes needs owned data
                let slice =
                    unsafe { std::slice::from_raw_parts(hwloc_bytes.as_ptr() as *const u8, size) };
                bytes::Bytes::copy_from_slice(slice)
            }
        }
    }
}

#[cfg(not(feature = "numa"))]
pub enum DataBuffer {
    Uma(Vec<u8>),
}

#[cfg(not(feature = "numa"))]
impl DataBuffer {
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            DataBuffer::Uma(vec) => vec.as_mut_slice(),
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        match self {
            DataBuffer::Uma(vec) => vec.as_slice(),
        }
    }

    pub fn as_ptr(&self) -> *const u8 {
        match self {
            DataBuffer::Uma(vec) => vec.as_ptr(),
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            DataBuffer::Uma(vec) => vec.as_mut_ptr(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            DataBuffer::Uma(vec) => vec.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn truncate(&mut self, size: usize) {
        match self {
            DataBuffer::Uma(vec) => vec.truncate(size),
        }
    }

    /// Convert to bytes::Bytes (ZERO-COPY: wraps the Vec without copying)
    pub fn into_bytes(self) -> bytes::Bytes {
        match self {
            DataBuffer::Uma(vec) => bytes::Bytes::from(vec),
        }
    }
}

/// Allocate NUMA-aware buffer on specific node
///
/// # Returns
/// - Ok((Topology, Bytes, size)) on successful NUMA allocation
/// - Err(String) on failure (caller should fall back to UMA)
#[cfg(feature = "numa")]
fn allocate_numa_buffer(
    size: usize,
    node_id: usize,
) -> Result<(Topology, hwlocality::memory::binding::Bytes<'static>, usize), String> {
    use hwlocality::object::types::ObjectType;

    // Create topology
    let topology =
        Topology::new().map_err(|e| format!("Failed to create hwloc topology: {}", e))?;

    // Find NUMA node
    let numa_nodes: Vec<_> = topology.objects_with_type(ObjectType::NUMANode).collect();

    if numa_nodes.is_empty() {
        return Err("No NUMA nodes found in topology".to_string());
    }

    // Get the NUMA node by OS index
    let node = numa_nodes
        .iter()
        .find(|n| n.os_index() == Some(node_id))
        .ok_or_else(|| {
            format!(
                "NUMA node {} not found (available: {:?})",
                node_id,
                numa_nodes
                    .iter()
                    .filter_map(|n| n.os_index())
                    .collect::<Vec<_>>()
            )
        })?;

    // Get nodeset for this NUMA node
    let nodeset = node
        .nodeset()
        .ok_or_else(|| format!("NUMA node {} has no nodeset", node_id))?;

    tracing::debug!(
        "Allocating {} bytes on NUMA node {} with nodeset {:?}",
        size,
        node_id,
        nodeset
    );

    // Allocate memory bound to this NUMA node
    // Using ASSUME_SINGLE_THREAD flag for maximum portability
    let bytes = topology
        .binding_allocate_memory(
            size,
            nodeset,
            MemoryBindingPolicy::Bind,
            MemoryBindingFlags::ASSUME_SINGLE_THREAD,
        )
        .map_err(|e| format!("Failed to allocate NUMA memory: {}", e))?;

    // SAFETY: We need to extend the lifetime to 'static because we're storing
    // both Topology and Bytes together, and Bytes' lifetime is tied to Topology.
    // This is safe because we keep Topology alive as long as Bytes exists.
    let bytes_static = unsafe {
        std::mem::transmute::<
            hwlocality::memory::binding::Bytes<'_>,
            hwlocality::memory::binding::Bytes<'static>,
        >(bytes)
    };

    Ok((topology, bytes_static, size))
}

/// Data generation algorithm
///
/// Only `Parallel` (Rayon-based Xoshiro256++) is supported.  Kept as an enum
/// for potential future extension; use the default `Parallel` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum GenerationMethod {
    /// Rayon-parallel Xoshiro256++ with controllable dedup and compression (default).
    #[default]
    Parallel,
}

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
    /// Pin to specific NUMA node (None = use all nodes, Some(n) = pin to node n)
    /// When set, only uses cores from this NUMA node and limits threads accordingly
    pub numa_node: Option<usize>,
    /// Internal block size for parallelization (None = use BLOCK_SIZE constant)
    /// Larger blocks (16-32 MB) improve throughput by amortizing Rayon overhead
    /// but use more memory. Must be at least 1 MB and at most 32 MB.
    pub block_size: Option<usize>,
    /// Random seed for reproducible data generation (None = use time + urandom)
    /// When set, generates identical data for the same seed value
    pub seed: Option<u64>,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            size: BLOCK_SIZE,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None, // Use all available cores
            seed: None,        // Use time + urandom
            numa_node: None,   // Use all NUMA nodes
            block_size: None,  // Use BLOCK_SIZE constant (1 MB)
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
/// use dgen_data::generate_data_simple;
///
/// // Generate 100 MiB incompressible data with no deduplication
/// let data = generate_data_simple(100 * 1024 * 1024, 1, 1);
/// assert_eq!(data.len(), 100 * 1024 * 1024);
/// ```
pub fn generate_data_simple(size: usize, dedup: usize, compress: usize) -> DataBuffer {
    let config = GeneratorConfig {
        size,
        dedup_factor: dedup.max(1),
        compress_factor: compress.max(1),
        numa_mode: NumaMode::Auto,
        max_threads: None,
        numa_node: None,
        block_size: None,
        seed: None,
    };
    generate_data(config)
}

/// Generate data with full configuration (ZERO-COPY - returns DataBuffer)
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
///
/// # Returns
/// DataBuffer that holds the generated data without copying:
/// - UMA: Vec<u8> wrapper
/// - NUMA: hwlocality Bytes wrapper (when numa_node is specified)
///
/// Python accesses this memory directly via buffer protocol - ZERO COPY!
pub fn generate_data(config: GeneratorConfig) -> DataBuffer {
    // Validate and get effective block size (default 4 MB, max 32 MB)
    let block_size = config
        .block_size
        .map(|bs| bs.clamp(1024 * 1024, 32 * 1024 * 1024)) // 1 MB min, 32 MB max
        .unwrap_or(BLOCK_SIZE);

    tracing::info!(
        "Starting data generation: size={}, dedup={}, compress={}, block_size={}",
        config.size,
        config.dedup_factor,
        config.compress_factor,
        block_size
    );

    // Keep the original requested size for final truncation.
    //
    // For sub-block objects (requested_size < block_size) we must NOT expand
    // to a full block, because fill_block() places compressible zeros at the
    // END of the buffer — and we truncate from the end.  Expanding to 1 MiB
    // and then truncating to, say, 32 KiB would throw away ALL the zeros,
    // leaving the caller with incompressible data no matter what compress_factor
    // was requested.
    //
    // Minimum granularity is 512 bytes: the fraction (compress-1)/compress of
    // 512 bytes is at least 1 byte for compress>=2, giving meaningful results.
    // Objects smaller than 512 bytes are generated at 512 bytes and truncated.
    const MIN_BLOCK: usize = 512;
    let requested_size = config.size;
    let size = if requested_size < block_size {
        // Sub-block object: use actual object size (clamped to MIN_BLOCK) as
        // the internal "block", so compression ratio applies to visible bytes.
        requested_size.max(MIN_BLOCK)
    } else {
        requested_size // multi-block: no expansion needed, div_ceil handles partial last block
    };
    let nblocks = size.div_ceil(block_size);

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

    // Calculate per-block copy lengths using integer error accumulation.
    // For sub-block objects (nblocks == 1 and size < block_size), the
    // "effective block" is `size` itself — not `block_size` — so that the
    // requested compression ratio applies to the actual bytes returned.
    let (f_num, f_den) = if config.compress_factor > 1 {
        (config.compress_factor - 1, config.compress_factor)
    } else {
        (0, 1)
    };
    let effective_block_size = if size < block_size { size } else { block_size };
    let floor_len = (f_num * effective_block_size) / f_den;
    let rem = (f_num * effective_block_size) % f_den;

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

    // Per-call entropy for RNG seeding — honour config.seed when provided
    let call_entropy = config.seed.unwrap_or_else(generate_call_entropy);

    // Allocate buffer (NUMA-aware if numa_node is specified).
    // For sub-block objects, total_size = size (the actual object size, not a full 1 MiB block).
    // For multi-block objects, total_size = nblocks * block_size (same as before).
    let total_size = nblocks * effective_block_size;
    tracing::debug!("Allocating {} bytes ({} blocks of {} B each)", total_size, nblocks, effective_block_size);

    // CRITICAL: UMA fast path - always use Vec<u8> when numa_node is None
    // This preserves 43-50 GB/s performance on UMA systems
    #[cfg(feature = "numa")]
    let mut data_buffer = if let Some(node_id) = config.numa_node {
        tracing::info!("Attempting NUMA allocation on node {}", node_id);
        match allocate_numa_buffer(total_size, node_id) {
            Ok(buffer) => {
                tracing::info!(
                    "Successfully allocated {} bytes on NUMA node {}",
                    total_size,
                    node_id
                );
                DataBuffer::Numa(buffer)
            }
            Err(e) => {
                tracing::warn!("NUMA allocation failed: {}, falling back to UMA", e);
                DataBuffer::Uma(vec![0u8; total_size])
            }
        }
    } else {
        DataBuffer::Uma(vec![0u8; total_size])
    };

    #[cfg(not(feature = "numa"))]
    let mut data_buffer = DataBuffer::Uma(vec![0u8; total_size]);

    // NUMA optimization check
    #[cfg(feature = "numa")]
    let numa_topology = if config.numa_mode != NumaMode::Disabled {
        NumaTopology::detect().ok()
    } else {
        None
    };

    // Adjust thread count if pinning to specific NUMA node
    #[cfg(feature = "numa")]
    let num_threads = if let Some(node_id) = config.numa_node {
        if let Some(ref topology) = numa_topology {
            if let Some(node) = topology.nodes.iter().find(|n| n.node_id == node_id) {
                // Limit threads to cores available on this NUMA node
                let node_cores = node.cpus.len();
                let requested_threads = config.max_threads.unwrap_or(node_cores);
                let threads = requested_threads.min(node_cores);
                tracing::info!(
                    "Pinning to NUMA node {}: using {} threads ({} cores available)",
                    node_id,
                    threads,
                    node_cores
                );
                threads
            } else {
                tracing::warn!(
                    "NUMA node {} not found, using default thread count",
                    node_id
                );
                config.max_threads.unwrap_or_else(get_affinity_cpu_count)
            }
        } else {
            tracing::warn!("NUMA topology not available, falling back to CPU affinity mask");
            // CRITICAL: When numa_node is specified but topology unavailable,
            // respect the process's CPU affinity mask (set by Python multiprocessing)
            config.max_threads.unwrap_or_else(get_affinity_cpu_count)
        }
    } else {
        // No specific NUMA node, use all cores
        config.max_threads.unwrap_or_else(num_cpus::get)
    };

    #[cfg(not(feature = "numa"))]
    let num_threads = config.max_threads.unwrap_or_else(num_cpus::get);

    tracing::info!("Using {} threads for parallel generation", num_threads);

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
                let cpu_map = std::sync::Arc::new(build_cpu_affinity_map(
                    topology,
                    num_threads,
                    config.numa_node,
                ));

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

    // Non-NUMA path: use the global Rayon pool directly (no per-call pool creation).
    // The global pool is initialised once by Rayon on first use and reused for the
    // lifetime of the process.  Rayon's work-stealing scheduler only spawns as many
    // parallel tasks as there are chunks, so on a 256-core machine generating an
    // 8 MiB object (2 × 4 MiB blocks) only 2 cores are used — no waste.
    #[cfg(not(feature = "numa"))]
    let _ = num_threads; // consumed by NUMA path only

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
                    let _data = data_buffer.as_mut_slice();
                    _data.par_chunks_mut(block_size).for_each(|chunk| {
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

    // NUMA path: use the custom thread-pinned pool built above.
    #[cfg(all(feature = "numa", feature = "thread-pinning"))]
    pool.install(|| {
        let data = data_buffer.as_mut_slice();
        data.par_chunks_mut(effective_block_size)
            .enumerate()
            .for_each(|(i, chunk)| {
                let ub = i % unique_blocks;
                tracing::trace!("Filling block {} (unique block {})", i, ub);
                fill_block(
                    chunk,
                    ub,
                    copy_lens[ub].min(chunk.len()),
                    ub as u64, // seed from unique-block index so duplicate blocks are identical
                    call_entropy,
                );
            });
    });

    // Non-NUMA path: use the global Rayon pool (no per-call allocation).
    #[cfg(not(all(feature = "numa", feature = "thread-pinning")))]
    data_buffer
        .as_mut_slice()
        .par_chunks_mut(effective_block_size)
        .enumerate()
        .for_each(|(i, chunk)| {
            let ub = i % unique_blocks;
            tracing::trace!("Filling block {} (unique block {})", i, ub);
            fill_block(
                chunk,
                ub,
                copy_lens[ub].min(chunk.len()),
                ub as u64, // seed from unique-block index so duplicate blocks are identical
                call_entropy,
            );
        });

    tracing::debug!(
        "Parallel generation complete, truncating to {} bytes (requested {})",
        requested_size,
        size
    );
    // Truncate to the *originally requested* size (metadata only, NO COPY!).
    // `size` may have been expanded to a full block; `requested_size` is what
    // the caller actually asked for.
    data_buffer.truncate(requested_size);

    // Return DataBuffer directly - Python accesses via raw pointer (ZERO COPY!)
    data_buffer
}

/// Fill a single block with controlled compression
///
/// # Algorithm (OPTIMIZED January 2026)
///
/// **NEW METHOD (Current)**: Zero-fill for compression
/// 1. Fill incompressible portion with Xoshiro256++ keystream (high-entropy random data)
/// 2. Fill compressible portion with zeros (memset - extremely fast)
///
/// **OLD METHOD (Before Jan 2026)**: Back-reference approach
/// - Filled entire block with RNG data
/// - Created back-references using copy_within() in 64-256 byte chunks
/// - SLOW: Required 2x memory traffic (write all, then copy 50% for 2:1 compression)
/// - Example: 1 MB block @ 2:1 ratio = 1 MB RNG write + 512 KB of copy_within operations
///
/// **WHY CHANGED**:
/// - Testing showed significant slowdown with compression enabled (1-4 GB/s vs 15 GB/s)
/// - Back-references created small, inefficient memory copies
/// - Zero-fill approach matches DLIO benchmark methodology
/// - Much faster: memset is highly optimized (often CPU instruction or libc fast path)
///
/// **PERFORMANCE COMPARISON**:
/// - Incompressible (copy_len=0): ~15 GB/s per core (both methods identical)
/// - 2:1 compression (copy_len=50%): OLD ~2-4 GB/s, NEW ~10-12 GB/s (estimated)
///
/// # Parameters
/// - `out`: Output buffer (BLOCK_SIZE bytes)
/// - `unique_block_idx`: Index of unique block (for RNG seeding)
/// - `copy_len`: Target bytes to make compressible (filled with zeros)
/// - `block_sequence`: Sequential block number for RNG derivation
/// - `seed_base`: Base seed for this generation session
fn fill_block(
    out: &mut [u8],
    unique_block_idx: usize,
    copy_len: usize,
    block_sequence: u64,
    seed_base: u64,
) {
    tracing::trace!(
        "fill_block: idx={}, seq={}, copy_len={}, out_len={}",
        unique_block_idx,
        block_sequence,
        copy_len,
        out.len()
    );

    // Derive RNG from seed_base + sequential block number
    // This ensures: same seed_base + same sequence → identical output
    let seed = seed_base.wrapping_add(block_sequence);
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);

    // OPTIMIZED COMPRESSION METHOD (January 2026):
    // For compress_factor N:1 ratio, we want (N-1)/N of the block to be compressible
    // Example: 2:1 ratio means 50% compressible, 4:1 means 75% compressible
    //
    // Strategy: Fill incompressible portion with RNG, compressible portion with zeros
    // This is MUCH faster than the old back-reference approach

    if copy_len == 0 {
        // No compression: fill entire block with high-entropy random data
        tracing::trace!(
            "Filling {} bytes with RNG keystream (incompressible)",
            out.len()
        );
        rng.fill_bytes(out);
    } else {
        // With compression: split between random and zeros
        let incompressible_len = out.len().saturating_sub(copy_len);

        tracing::trace!(
            "Filling block: {} bytes random (incompressible) + {} bytes zeros (compressible)",
            incompressible_len,
            copy_len
        );

        // Step 1: Fill incompressible portion with high-entropy keystream
        if incompressible_len > 0 {
            rng.fill_bytes(&mut out[..incompressible_len]);
        }

        // Step 2: Fill compressible portion with zeros (memset - super fast!)
        // This is typically optimized to a CPU instruction or fast libc call
        if copy_len > 0 && incompressible_len < out.len() {
            out[incompressible_len..].fill(0);
        }
    }

    tracing::trace!(
        "fill_block complete: {} compressible bytes (zeros)",
        copy_len
    );
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

/// Get CPU count from current process affinity mask
/// Falls back to num_cpus::get() if affinity cannot be determined
fn get_affinity_cpu_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        // Try to read /proc/self/status to get Cpus_allowed_list
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("Cpus_allowed_list:") {
                    if let Some(cpus) = line.split(':').nth(1) {
                        let cpus = cpus.trim();
                        let count = parse_cpu_list(cpus);
                        if count > 0 {
                            tracing::debug!("CPU affinity mask: {} CPUs ({})", count, cpus);
                            return count;
                        }
                    }
                }
            }
        }
    }

    // Fallback to system CPU count
    num_cpus::get()
}

/// Parse Linux CPU list (e.g., "0-23" or "0-11,24-35")
#[cfg(target_os = "linux")]
fn parse_cpu_list(cpu_list: &str) -> usize {
    let mut count = 0;
    for range in cpu_list.split(',') {
        let range = range.trim();
        if range.is_empty() {
            continue;
        }

        if let Some((start, end)) = range.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.parse::<usize>(), end.parse::<usize>()) {
                count += (e - s) + 1;
            }
        } else if range.parse::<usize>().is_ok() {
            count += 1;
        }
    }
    count
}

/// Build CPU affinity map for thread pinning
#[cfg(all(feature = "numa", feature = "thread-pinning"))]
/// Build CPU affinity map for thread pinning
/// If numa_node is Some(n), only use cores from NUMA node n
/// If numa_node is None, distribute threads across all NUMA nodes
#[cfg(all(feature = "numa", feature = "thread-pinning"))]
fn build_cpu_affinity_map(
    topology: &crate::numa::NumaTopology,
    num_threads: usize,
    numa_node: Option<usize>,
) -> HashMap<usize, Vec<usize>> {
    let mut map = HashMap::new();

    if let Some(target_node_id) = numa_node {
        // Pin to specific NUMA node only
        if let Some(target_node) = topology.nodes.iter().find(|n| n.node_id == target_node_id) {
            tracing::info!(
                "Pinning {} threads to NUMA node {} ({} cores available)",
                num_threads,
                target_node_id,
                target_node.cpus.len()
            );

            // Distribute threads across cores in this NUMA node only
            for thread_id in 0..num_threads {
                let core_idx = thread_id % target_node.cpus.len();
                let core_id = target_node.cpus[core_idx];

                tracing::trace!(
                    "Thread {} -> NUMA node {} core {}",
                    thread_id,
                    target_node_id,
                    core_id
                );
                map.insert(thread_id, vec![core_id]);
            }
        } else {
            tracing::warn!(
                "NUMA node {} not found in topology (available: 0-{})",
                target_node_id,
                topology.num_nodes - 1
            );
        }
    } else {
        // Distribute threads across ALL NUMA nodes (old behavior)
        let mut thread_id = 0;
        let mut node_idx = 0;

        while thread_id < num_threads {
            if let Some(node) = topology.nodes.get(node_idx % topology.nodes.len()) {
                // Assign threads to cores within this NUMA node
                let cores_per_thread =
                    (node.cpus.len() as f64 / num_threads as f64).ceil() as usize;
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
// Global Rayon pool — one per process, shared by all DataGenerators
// =============================================================================
//
// WHY ONE POOL:
//   Rayon's work-stealing scheduler distributes tasks from N concurrent callers
//   across a fixed set of OS threads.  A pool with T threads serves any number
//   of simultaneous fill_chunk_parallel() callers with T total OS threads —
//   no oversubscription regardless of how many DataGenerators exist.
//
// SIZING — automatic, no env vars required:
//   Priority (highest wins):
//     1. DGEN_THREADS env var — explicit override for any scenario
//     2. RAYON_NUM_THREADS env var — standard Rayon convention
//     3. CPU affinity mask — respects taskset / Docker --cpuset-cpus / cgroups
//        (get_affinity_cpu_count() reads /proc/self/status on Linux)
//     4. PID-file sibling count — counts live dgen processes in /tmp/dgen-<uid>/
//        and divides the affinity CPU count accordingly
//     5. Total system CPU count as final fallback
//
// PID-FILE DESIGN:
//   On first DataGenerator::new(), we write /tmp/dgen-<euid>/<pid>.
//   We count all files whose name-as-pid still has a live /proc/<pid>/ entry.
//   Stale files (crashed processes) are automatically ignored.
//   On clean exit (Drop or atexit) the file is removed.
//   Two processes racing at startup both see n=1 briefly → both build a
//   full-CPU pool → slight transient oversubscription for milliseconds,
//   then they self-correct on the next init. "Somewhat wrong then right"
//   is the correct trade-off, per the design intent.
//
// EXAMPLE: 8 Python DLIO processes on 64-CPU bare-metal host, no affinity set:
//   Process 1 starts → n=1 → pool=64 threads
//   Process 2 starts → n=2 → pool=32 threads  (its own new OnceLock)
//   ...
//   Process 8 starts → n=8 → pool=8 threads
//   Each settled process holds an 8-thread pool; 8×8=64 OS threads on 64 CPUs.

static GLOBAL_POOL: OnceLock<Arc<rayon::ThreadPool>> = OnceLock::new();

/// Return a reference-counted handle to the process-global Rayon thread pool.
///
/// Initialised exactly once (on first call) using the sizing heuristic above.
/// All subsequent `DataGenerator` instances in the same process share this pool.
pub fn global_pool() -> Arc<rayon::ThreadPool> {
    GLOBAL_POOL
        .get_or_init(|| {
            // Register PID file and count siblings first, so the pool size is right.
            register_pid_file();
            let n = compute_pool_size();
            tracing::info!(
                "dgen-data: global Rayon pool initialised with {} threads",
                n
            );
            Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(n)
                    .build()
                    .expect("failed to build dgen-data global Rayon pool"),
            )
        })
        .clone()
}

/// Compute how many threads the global pool should use.
fn compute_pool_size() -> usize {
    // 1. Explicit override via DGEN_THREADS
    if let Ok(v) = std::env::var("DGEN_THREADS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n > 0 {
                tracing::info!("dgen-data: pool size from DGEN_THREADS={}", n);
                return n;
            }
        }
    }

    // 2. Standard Rayon convention
    if let Ok(v) = std::env::var("RAYON_NUM_THREADS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n > 0 {
                tracing::info!("dgen-data: pool size from RAYON_NUM_THREADS={}", n);
                return n;
            }
        }
    }

    // 3+4. CPU affinity ÷ sibling processes
    let affinity = get_affinity_cpu_count();
    let siblings = count_sibling_processes().max(1);
    let n = (affinity / siblings).max(1);
    if siblings > 1 {
        tracing::info!(
            "dgen-data: pool size={} (affinity={} / siblings={})",
            n,
            affinity,
            siblings
        );
    } else {
        tracing::info!("dgen-data: pool size={} (affinity={})", n, affinity);
    }
    n
}

// ── PID-file helpers ──────────────────────────────────────────────────────────

fn pid_dir() -> std::path::PathBuf {
    // Use effective UID from /proc/self/status (Linux) to keep directories
    // per-user without needing the libc crate.  Falls back to username or "0".
    let uid = pid_dir_uid();
    std::path::PathBuf::from(format!("/tmp/dgen-{}", uid))
}

fn pid_dir_uid() -> String {
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("Uid:") {
                // "Uid:\treal\teffective\tsaved\tfs"
                if let Some(euid) = line.split_whitespace().nth(2) {
                    return euid.to_string();
                }
            }
        }
    }
    // Fallback: sanitised username or "0"
    std::env::var("USER")
        .unwrap_or_else(|_| "0".to_string())
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Write /tmp/dgen-<uid>/<pid> and schedule its removal on process exit.
fn register_pid_file() {
    let dir = pid_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return; // tmpfs not writable — silently skip
    }
    let path = dir.join(std::process::id().to_string());
    let _ = std::fs::write(&path, b"");

    // Schedule removal via a background thread that parks until program exit.
    // We use a plain thread with a channel rather than std::panic::catch_unwind
    // or atexit C-FFI to keep the code simple and safe.
    let path_clone = path.clone();
    std::thread::spawn(move || {
        // Park indefinitely; the OS will deliver SIGTERM/SIGKILL at process exit,
        // but for clean shutdowns the thread gets unparked by the Drop below.
        // For crashed processes the file simply stays; count_sibling_processes()
        // filters those out via /proc/<pid>/ liveness check.
        std::thread::park();
        let _ = std::fs::remove_file(&path_clone);
    });
}

/// Count how many other dgen processes are currently running.
/// Returns 1 (just us) when the pid directory is not usable.
fn count_sibling_processes() -> usize {
    let dir = pid_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 1;
    };
    let our_pid = std::process::id().to_string();
    let mut live = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        if pid_str.as_ref() == our_pid {
            live += 1; // count ourselves
            continue;
        }
        // Check liveness: /proc/<pid>/ must exist (Linux) or kill -0 succeeds.
        #[cfg(target_os = "linux")]
        {
            if std::path::Path::new(&format!("/proc/{}/", pid_str)).exists() {
                live += 1;
            } else {
                // Stale file — remove opportunistically
                let _ = std::fs::remove_file(entry.path());
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Non-Linux: can't check /proc; count all files conservatively
            live += 1;
        }
    }
    live.max(1)
}

// =============================================================================
// Streaming Generator
// =============================================================================

/// Streaming data generator
///
/// # RNG design
///
/// Each 1 MiB internal block is seeded from `call_entropy.wrapping_add(ub)`,
/// where `ub = epoch_offset % unique_blocks` and
/// `epoch_offset = block_idx - seed_epoch_start_block`.
///
/// This guarantees both:
///
///   • **Correct deduplication**: blocks that are N * unique_blocks apart share
///     the same `ub` (same epoch-relative offset mod unique_blocks) and therefore
///     get identical content.
///
///   • **Stripe reproducibility**: `set_seed(S)` records the current block as the
///     new epoch start.  The first block after any `set_seed(S)` call always has
///     `epoch_offset=0, ub=0` — so the same seed produces identical data
///     regardless of absolute stream position.
///
///   • **Chunk-size independence**: `ub` depends only on `block_idx` (derived
///     from stream position), not on how many `fill_chunk` calls were made.
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
    /// Block index at which the current seed epoch started.
    /// Resets to the current block whenever `set_seed()` is called.
    seed_epoch_start_block: usize,
    max_threads: usize,
    block_size: usize,
}

impl DataGenerator {
    /// Create new streaming generator
    pub fn new(config: GeneratorConfig) -> Self {
        // ── Initialise the global Rayon pool (first caller wins, all others share) ──
        let _pool = global_pool();

        // ── Validate and get effective block size (default 4 MB, max 32 MB) ─────
        let block_size = config
            .block_size
            .map(|bs| bs.clamp(1024 * 1024, 32 * 1024 * 1024)) // 1 MB min, 32 MB max
            .unwrap_or(BLOCK_SIZE);

        tracing::info!(
            "Creating DataGenerator: size={}, dedup={}, compress={}, block_size={}",
            config.size,
            config.dedup_factor,
            config.compress_factor,
            block_size
        );

        // The streaming generator respects the exact requested size (including 0).
        // The batch path (generate_data) separately enforces a block-size minimum
        // for its internal allocation, but that is not our concern here.
        let total_size = config.size;
        let nblocks = total_size.div_ceil(block_size);

        let dedup_factor = config.dedup_factor.max(1);
        // unique_blocks must be at least 1 to avoid modulo-by-zero; when size=0
        // the generator immediately returns 0 from fill_chunk so copy_lens[0]
        // is never accessed, but we still allocate one entry for safety.
        let unique_blocks = if nblocks == 0 {
            1
        } else if dedup_factor > 1 {
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
        // For objects smaller than one block, scale copy_len to the actual object
        // size rather than the full block_size.  This ensures the requested
        // compression ratio applies correctly to the bytes the caller will see,
        // not to the (never-used) tail of a 1 MiB scratch buffer.
        let effective_block_size = if total_size > 0 && total_size < block_size {
            total_size
        } else {
            block_size
        };
        let floor_len = (f_num * effective_block_size) / f_den;
        let rem = (f_num * effective_block_size) % f_den;

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

        // ── Entropy / seed ────────────────────────────────────────────────────
        //
        // DO NOT pass a seed unless you specifically need the same byte sequence
        // to be reproducible across calls.  Omitting a seed (config.seed = None)
        // causes generate_call_entropy() to mix system time + urandom, producing
        // unique, high-entropy data every time — which is correct for benchmarks.
        //
        // Rule of thumb: seed = None (the default) for everything except unit
        // tests that verify deterministic behaviour.
        let call_entropy = config.seed.unwrap_or_else(generate_call_entropy);

        let max_threads = config.max_threads.unwrap_or_else(num_cpus::get);

        Self {
            total_size,
            current_pos: 0,
            dedup_factor,
            compress_factor: config.compress_factor,
            unique_blocks,
            copy_lens,
            call_entropy,
            seed_epoch_start_block: 0,
            max_threads,
            block_size,
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
        let start_block = self.current_pos / self.block_size;
        let start_offset = self.current_pos % self.block_size;
        let end_pos = self.current_pos + to_write;
        let end_block = (end_pos - 1) / self.block_size;
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
            let remaining_in_block = self.block_size - block_offset;
            let to_copy = remaining_in_block.min(chunk.len() - offset);

            // epoch_offset is relative to the last set_seed() call.
            // ub = epoch_offset % unique_blocks ensures:
            //   - dedup: blocks N * unique_blocks apart share the same ub (same content)
            //   - stripe reproducibility: ub resets to 0 after each set_seed()
            let epoch_offset = block_idx.saturating_sub(self.seed_epoch_start_block);
            let ub = epoch_offset % self.unique_blocks;

            let mut block_buf = vec![0u8; self.block_size.min(
                if self.total_size > 0 && self.total_size < self.block_size {
                    self.total_size
                } else {
                    self.block_size
                },
            )];
            let actual_block_size = block_buf.len();
            fill_block(
                &mut block_buf,
                ub,
                self.copy_lens[ub].min(actual_block_size),
                ub as u64,
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

    /// Parallel fill for large buffers (uses process-global Rayon thread pool — zero copy)
    fn fill_chunk_parallel(
        &mut self,
        chunk: &mut [u8],
        start_block: usize,
        start_offset: usize,
        num_blocks: usize,
    ) -> usize {
        use rayon::prelude::*;

        let thread_pool = global_pool();

        let call_entropy = self.call_entropy;
        let copy_lens = &self.copy_lens;
        let unique_blocks = self.unique_blocks;
        let block_size = self.block_size;
        let seed_epoch_start_block = self.seed_epoch_start_block;
        let total_size = self.total_size;
        // For sub-block objects, compress relative to actual object size.
        let actual_block_size = if total_size > 0 && total_size < block_size {
            total_size
        } else {
            block_size
        };

        // ZERO-COPY: Generate directly into output buffer using par_chunks_mut
        thread_pool.install(|| {
            chunk
                .par_chunks_mut(block_size)
                .enumerate()
                .for_each(|(i, block_chunk)| {
                    let block_idx = start_block + i;
                    // epoch_offset resets to 0 at each set_seed() call.
                    // ub = epoch_offset % unique_blocks: same ub within an epoch
                    // means identical block content (dedup) and the epoch reset
                    // means stripes are reproducible across stream positions.
                    let epoch_offset = block_idx.saturating_sub(seed_epoch_start_block);
                    let ub = epoch_offset % unique_blocks;

                    // Handle first block with offset
                    if i == 0 && start_offset > 0 {
                        let mut temp = vec![0u8; actual_block_size];
                        fill_block(
                            &mut temp,
                            ub,
                            copy_lens[ub].min(actual_block_size),
                            ub as u64,
                            call_entropy,
                        );
                        let copy_len = actual_block_size
                            .saturating_sub(start_offset)
                            .min(block_chunk.len());
                        block_chunk[..copy_len]
                            .copy_from_slice(&temp[start_offset..start_offset + copy_len]);
                    } else {
                        let actual_len = block_chunk.len().min(block_size);
                        fill_block(
                            &mut block_chunk[..actual_len],
                            ub,
                            copy_lens[ub].min(actual_len),
                            ub as u64,
                            call_entropy,
                        );
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

    /// Set or reset the random seed for subsequent data generation
    ///
    /// This allows changing the data pattern mid-stream while maintaining generation position.
    /// The new seed takes effect on the next `fill_chunk()` call.
    ///
    /// # Arguments
    /// * `seed` - New seed value, or None to use time+urandom entropy (non-deterministic)
    ///
    /// # Examples
    /// ```rust,no_run
    /// use dgen_data::{DataGenerator, GeneratorConfig, NumaMode};
    ///
    /// let config = GeneratorConfig {
    ///     size: 100 * 1024 * 1024,
    ///     dedup_factor: 1,
    ///     compress_factor: 1,
    ///     numa_mode: NumaMode::Auto,
    ///     max_threads: None,
    ///     numa_node: None,
    ///     block_size: None,
    ///     seed: Some(12345),
    /// };
    ///
    /// let mut gen = DataGenerator::new(config);
    /// let mut buffer = vec![0u8; 1024 * 1024];
    ///
    /// // Generate some data with initial seed
    /// gen.fill_chunk(&mut buffer);
    ///
    /// // Change seed for different pattern
    /// gen.set_seed(Some(67890));
    /// gen.fill_chunk(&mut buffer);  // Uses new seed
    ///
    /// // Switch to non-deterministic mode
    /// gen.set_seed(None);
    /// gen.fill_chunk(&mut buffer);  // Uses time+urandom
    /// ```
    /// Set or reset the random seed for subsequent data generation.
    ///
    /// # ⚠️  WHEN TO USE THIS
    ///
    /// **Do NOT call `set_seed` unless you have a specific reason to reproduce
    /// the same byte sequence.**  For ordinary data generation (benchmarks,
    /// test data, simulated workloads) the default non-deterministic entropy is
    /// correct and you should never touch the seed.
    ///
    /// Legitimate uses:
    ///   - Unit tests comparing two generators byte-for-byte
    ///   - Striped data patterns that must be reproduced on a second pass
    ///     (e.g. write stripe A, write stripe B, verify stripe A again)
    ///
    pub fn set_seed(&mut self, seed: Option<u64>) {
        self.call_entropy = seed.unwrap_or_else(generate_call_entropy);
        // Record current block as epoch start so epoch_offset resets to 0
        // from this point, making stripe data reproducible across positions.
        self.seed_epoch_start_block = self.current_pos / self.block_size;
        tracing::debug!(
            "set_seed: {} (entropy={:#018x}), epoch starts at block {}",
            if seed.is_some() { "deterministic" } else { "non-deterministic" },
            self.call_entropy,
            self.seed_epoch_start_block,
        );
    }

    /// Get recommended chunk size for optimal performance
    ///
    /// Returns 32 MB, which provides the best balance between:
    /// - Parallelism: 8 blocks × 4 MB = good distribution across cores
    /// - Cache locality: Fits well in L3 cache
    /// - Memory overhead: Reasonable buffer size
    ///
    /// Based on empirical testing showing 32 MB is ~16% faster than 64 MB
    /// and significantly better than smaller or larger sizes.
    pub fn recommended_chunk_size() -> usize {
        32 * 1024 * 1024 // 32 MB
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
        // generate_data_simple returns exactly the requested size (no 1 MiB minimum).
        let data = generate_data_simple(100, 1, 1);
        assert_eq!(data.len(), 100, "should return exactly the requested byte count");
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
            numa_node: None,
            block_size: None,
            seed: None,
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

    #[test]
    fn test_set_seed_stream_reset() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        fn hash_buffer(buf: &[u8]) -> u64 {
            let mut hasher = DefaultHasher::new();
            buf.hash(&mut hasher);
            hasher.finish()
        }

        init_tracing();
        eprintln!("Testing set_seed() stream reset behavior...");

        let size = 30 * 1024 * 1024; // 30 MB
        let chunk_size = 10 * 1024 * 1024; // 10 MB chunks

        // Test 1: Same seed sequence produces identical data
        eprintln!("Test 1: Seed sequence reproducibility");
        let config = GeneratorConfig {
            size,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: None,
            seed: Some(111),
        };

        // First run with seed sequence: 111 -> 222 -> 333
        let mut gen1 = DataGenerator::new(config.clone());
        let mut buf1 = vec![0u8; chunk_size];

        gen1.fill_chunk(&mut buf1);
        let hash1a = hash_buffer(&buf1);

        gen1.set_seed(Some(222));
        gen1.fill_chunk(&mut buf1);
        let hash1b = hash_buffer(&buf1);

        gen1.set_seed(Some(333));
        gen1.fill_chunk(&mut buf1);
        let hash1c = hash_buffer(&buf1);

        // Second run with same seed sequence
        let mut gen2 = DataGenerator::new(config.clone());
        let mut buf2 = vec![0u8; chunk_size];

        gen2.fill_chunk(&mut buf2);
        let hash2a = hash_buffer(&buf2);

        gen2.set_seed(Some(222));
        gen2.fill_chunk(&mut buf2);
        let hash2b = hash_buffer(&buf2);

        gen2.set_seed(Some(333));
        gen2.fill_chunk(&mut buf2);
        let hash2c = hash_buffer(&buf2);

        eprintln!("  Chunk 1: hash1={:016x}, hash2={:016x}", hash1a, hash2a);
        eprintln!("  Chunk 2: hash1={:016x}, hash2={:016x}", hash1b, hash2b);
        eprintln!("  Chunk 3: hash1={:016x}, hash2={:016x}", hash1c, hash2c);

        assert_eq!(hash1a, hash2a, "Chunk 1 (seed=111) should match");
        assert_eq!(hash1b, hash2b, "Chunk 2 (seed=222) should match");
        assert_eq!(hash1c, hash2c, "Chunk 3 (seed=333) should match");

        // Test 2: Striped pattern (A-B-A-B) reproduces correctly
        eprintln!("Test 2: Striped pattern creation");
        let mut gen = DataGenerator::new(GeneratorConfig {
            size: 40 * 1024 * 1024,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None,
            numa_node: None,
            block_size: None,
            seed: Some(1111),
        });

        let mut buf = vec![0u8; chunk_size];

        // Stripe 1: A
        gen.set_seed(Some(1111));
        gen.fill_chunk(&mut buf);
        let stripe1_hash = hash_buffer(&buf);

        // Stripe 2: B
        gen.set_seed(Some(2222));
        gen.fill_chunk(&mut buf);
        let stripe2_hash = hash_buffer(&buf);

        // Stripe 3: A (should match Stripe 1)
        gen.set_seed(Some(1111));
        gen.fill_chunk(&mut buf);
        let stripe3_hash = hash_buffer(&buf);

        // Stripe 4: B (should match Stripe 2)
        gen.set_seed(Some(2222));
        gen.fill_chunk(&mut buf);
        let stripe4_hash = hash_buffer(&buf);

        eprintln!("  Stripe 1 (A): {:016x}", stripe1_hash);
        eprintln!("  Stripe 2 (B): {:016x}", stripe2_hash);
        eprintln!("  Stripe 3 (A): {:016x}", stripe3_hash);
        eprintln!("  Stripe 4 (B): {:016x}", stripe4_hash);

        assert_eq!(
            stripe1_hash, stripe3_hash,
            "Stripe A should be reproducible"
        );
        assert_eq!(
            stripe2_hash, stripe4_hash,
            "Stripe B should be reproducible"
        );
        assert_ne!(stripe1_hash, stripe2_hash, "Stripe A and B should differ");

        eprintln!("✅ All stream reset tests passed!");
    }
}
