// src/generator.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! High-performance data generation with controllable deduplication and compression
//!
//! Ported from s3dlio/src/data_gen_alt.rs with NUMA optimizations

use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use rayon::prelude::*;
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

    pub fn truncate(&mut self, size: usize) {
        match self {
            DataBuffer::Uma(vec) => vec.truncate(size),
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
            block_size: None,  // Use BLOCK_SIZE constant (4 MB)
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

    let size = config.size.max(block_size); // Use block_size as minimum
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

    // Calculate per-block copy lengths using integer error accumulation
    // This ensures even distribution of compression across blocks
    let (f_num, f_den) = if config.compress_factor > 1 {
        (config.compress_factor - 1, config.compress_factor)
    } else {
        (0, 1)
    };
    let floor_len = (f_num * block_size) / f_den;
    let rem = (f_num * block_size) % f_den;

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

    // Allocate buffer (NUMA-aware if numa_node is specified)
    let total_size = nblocks * block_size;
    tracing::debug!("Allocating {} bytes ({} blocks)", total_size, nblocks);

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

    pool.install(|| {
        let data = data_buffer.as_mut_slice();
        data.par_chunks_mut(block_size)
            .enumerate()
            .for_each(|(i, chunk)| {
                let ub = i % unique_blocks;
                tracing::trace!("Filling block {} (unique block {})", i, ub);
                // Use sequential block index for reproducibility
                fill_block(
                    chunk,
                    ub,
                    copy_lens[ub].min(chunk.len()),
                    i as u64,
                    call_entropy,
                );
            });
    });

    tracing::debug!("Parallel generation complete, truncating to {} bytes", size);
    // Truncate to requested size (metadata only, NO COPY!)
    data_buffer.truncate(size);

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
    block_sequence: u64, // Sequential counter for RNG derivation (reset by set_seed)
    max_threads: usize,  // Thread count for parallel generation
    thread_pool: Option<rayon::ThreadPool>, // Reused thread pool (created once)
    block_size: usize,   // Internal parallelization block size (4-32 MB)
}

impl DataGenerator {
    /// Create new streaming generator
    pub fn new(config: GeneratorConfig) -> Self {
        // Validate and get effective block size (default 4 MB, max 32 MB)
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

        let total_size = config.size.max(block_size); // Use block_size as minimum
        let nblocks = total_size.div_ceil(block_size);

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
        let floor_len = (f_num * block_size) / f_den;
        let rem = (f_num * block_size) % f_den;

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

        // Use provided seed or generate entropy from time + urandom
        let call_entropy = config.seed.unwrap_or_else(generate_call_entropy);

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
            block_sequence: 0, // Start at block 0
            max_threads,
            thread_pool,
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

            // Map to unique block
            let ub = block_idx % self.unique_blocks;

            // Generate full block
            let mut block_buf = vec![0u8; self.block_size];
            fill_block(
                &mut block_buf,
                ub,
                self.copy_lens[ub].min(self.block_size),
                self.block_sequence, // Use current sequence
                self.call_entropy,
            );

            self.block_sequence += 1; // Increment for next block

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
        let block_size = self.block_size;
        let base_sequence = self.block_sequence; // Capture current sequence

        // ZERO-COPY: Generate directly into output buffer using par_chunks_mut
        // This is the same approach as generate_data() - no temporary allocations!
        thread_pool.install(|| {
            chunk
                .par_chunks_mut(block_size)
                .enumerate()
                .for_each(|(i, block_chunk)| {
                    let block_idx = start_block + i;
                    let ub = block_idx % unique_blocks;
                    let block_seq = base_sequence + (i as u64); // Sequential block number

                    // Handle first block with offset
                    if i == 0 && start_offset > 0 {
                        // Generate full block into temp, copy needed portion
                        let mut temp = vec![0u8; block_size];
                        fill_block(
                            &mut temp,
                            ub,
                            copy_lens[ub].min(block_size),
                            block_seq,
                            call_entropy,
                        );
                        let copy_len = block_size
                            .saturating_sub(start_offset)
                            .min(block_chunk.len());
                        block_chunk[..copy_len]
                            .copy_from_slice(&temp[start_offset..start_offset + copy_len]);
                    } else {
                        // Generate directly into output buffer (ZERO-COPY!)
                        let actual_len = block_chunk.len().min(block_size);
                        fill_block(
                            &mut block_chunk[..actual_len],
                            ub,
                            copy_lens[ub].min(actual_len),
                            block_seq,
                            call_entropy,
                        );
                    }
                });
        });

        let to_write = chunk.len();
        self.current_pos += to_write;
        self.block_sequence += num_blocks as u64; // Increment sequence for next fill

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
    /// use dgen_rs::{DataGenerator, GeneratorConfig, NumaMode};
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
    pub fn set_seed(&mut self, seed: Option<u64>) {
        self.call_entropy = seed.unwrap_or_else(generate_call_entropy);
        // Reset block sequence counter - this ensures same seed → identical stream
        self.block_sequence = 0;
        tracing::debug!(
            "Seed reset: {} (entropy={}) - block_sequence reset to 0",
            if seed.is_some() {
                "deterministic"
            } else {
                "non-deterministic"
            },
            self.call_entropy
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
