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
                    std::slice::from_raw_parts_mut(
                        bytes.as_mut_ptr() as *mut u8,
                        bytes.len()
                    )
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
                unsafe {
                    std::slice::from_raw_parts(
                        bytes.as_ptr() as *const u8,
                        *size
                    )
                }
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
                let slice = unsafe {
                    std::slice::from_raw_parts(
                        hwloc_bytes.as_ptr() as *const u8,
                        size
                    )
                };
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
    let topology = Topology::new()
        .map_err(|e| format!("Failed to create hwloc topology: {}", e))?;
    
    // Find NUMA node
    let numa_nodes: Vec<_> = topology
        .objects_with_type(ObjectType::NUMANode)
        .collect();
    
    if numa_nodes.is_empty() {
        return Err("No NUMA nodes found in topology".to_string());
    }
    
    // Get the NUMA node by OS index
    let node = numa_nodes
        .iter()
        .find(|n| n.os_index() == Some(node_id))
        .ok_or_else(|| format!("NUMA node {} not found (available: {:?})", 
                               node_id,
                               numa_nodes.iter().filter_map(|n| n.os_index()).collect::<Vec<_>>()))?;
    
    // Get nodeset for this NUMA node
    let nodeset = node.nodeset()
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
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            size: BLOCK_SIZE,
            dedup_factor: 1,
            compress_factor: 1,
            numa_mode: NumaMode::Auto,
            max_threads: None, // Use all available cores
            numa_node: None,   // Use all NUMA nodes
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

    // Allocate buffer (NUMA-aware if numa_node is specified)
    let total_size = nblocks * BLOCK_SIZE;
    tracing::debug!("Allocating {} bytes ({} blocks)", total_size, nblocks);
    
    // CRITICAL: UMA fast path - always use Vec<u8> when numa_node is None
    // This preserves 43-50 GB/s performance on UMA systems
    #[cfg(feature = "numa")]
    let mut data_buffer = if let Some(node_id) = config.numa_node {
        tracing::info!("Attempting NUMA allocation on node {}", node_id);
        match allocate_numa_buffer(total_size, node_id) {
            Ok(buffer) => {
                tracing::info!("Successfully allocated {} bytes on NUMA node {}", total_size, node_id);
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
                config.max_threads.unwrap_or_else(num_cpus::get)
            }
        } else {
            tracing::warn!("NUMA topology not available, ignoring numa_node parameter");
            config.max_threads.unwrap_or_else(num_cpus::get)
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
                let cpu_map = std::sync::Arc::new(build_cpu_affinity_map(topology, num_threads, config.numa_node));

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
                    _data.par_chunks_mut(BLOCK_SIZE).for_each(|chunk| {
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
        data.par_chunks_mut(BLOCK_SIZE)
            .enumerate()
            .for_each(|(i, chunk)| {
                let ub = i % unique_blocks;
                tracing::trace!("Filling block {} (unique block {})", i, ub);
                fill_block(chunk, ub, copy_lens[ub].min(chunk.len()), call_entropy);
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
        32 * 1024 * 1024  // 32 MB
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
