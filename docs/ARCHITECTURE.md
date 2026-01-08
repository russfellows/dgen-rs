# dgen-rs Architecture

## Overview

dgen-rs is a high-performance data generation library designed for storage benchmarking and testing. It provides controllable deduplication and compression characteristics while maintaining 5-15 GB/s per-core throughput.

## Core Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Python API (Optional)                    │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────┐   │
│  │ Simple API  │  │ Zero-Copy API │  │ Streaming API    │   │
│  │ generate()  │  │ fill_buffer() │  │ Generator class  │   │
│  └──────┬──────┘  └──────┬───────┘  └────────┬─────────┘   │
│         │                 │                    │              │
│         └─────────────────┴────────────────────┘              │
│                           │                                   │
│                    PyO3 Bindings                              │
│                    (python_api.rs)                            │
└─────────────────────────────┬───────────────────────────────┘
                              │
┌─────────────────────────────┴───────────────────────────────┐
│                      Rust Core Library                       │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐  │
│  │               GeneratorConfig                         │  │
│  │  - size, dedup_factor, compress_factor               │  │
│  │  - numa_mode, max_threads                            │  │
│  └────────────────────┬─────────────────────────────────┘  │
│                       │                                      │
│  ┌────────────────────┴─────────────────────────────────┐  │
│  │            Data Generation Pipeline                   │  │
│  │                                                        │  │
│  │  1. Calculate unique blocks (dedup)                   │  │
│  │  2. Calculate copy lengths (compress)                 │  │
│  │  3. Generate per-call entropy                         │  │
│  │  4. Configure thread pool (NUMA-aware)                │  │
│  │  5. Parallel block generation (rayon)                 │  │
│  │  6. Truncate to requested size                        │  │
│  └──────────────────┬─────────────────────────────────┬──┘  │
│                     │                                  │      │
│  ┌──────────────────┴──────────┐  ┌──────────────────┴───┐ │
│  │   Block Generator            │  │  NUMA Topology       │ │
│  │   (fill_block)               │  │  (numa.rs)           │ │
│  │                              │  │                      │ │
│  │  1. Seed Xoshiro256++        │  │  - Detect nodes     │ │
│  │  2. Fill with RNG keystream  │  │  - CPU mapping      │ │
│  │  3. Add back-references      │  │  - Memory info      │ │
│  └──────────────────────────────┘  └──────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

## Data Generation Algorithm

### Phase 1: Configuration
```rust
GeneratorConfig {
    size: 100_000_000,           // 100 MB
    dedup_factor: 2,              // 2:1 dedup (50 unique blocks)
    compress_factor: 3,           // 3:1 compress ratio
    numa_mode: NumaMode::Auto,    // Auto-detect NUMA
    max_threads: None,            // Use all cores
}
```

### Phase 2: Deduplication Calculation
```
Total blocks = ceil(size / BLOCK_SIZE)
             = ceil(100_000_000 / 4_194_304) 
             = 24 blocks

Unique blocks = round(total_blocks / dedup_factor)
              = round(24 / 2)
              = 12 unique blocks

Block mapping (round-robin):
  Block 0  -> Unique Block 0
  Block 1  -> Unique Block 1
  ...
  Block 11 -> Unique Block 11
  Block 12 -> Unique Block 0  (reused!)
  Block 13 -> Unique Block 1  (reused!)
  ...
```

**Result**: 12 unique 4 MiB blocks generate 24 total blocks = 2:1 dedup ratio

### Phase 3: Compression Calculation

Uses **integer error accumulation** for even distribution:

```rust
// For compress_factor = 3
f_num = compress_factor - 1 = 2
f_den = compress_factor = 3

// Base copy length per block
floor_len = (2 * 4_194_304) / 3 = 2_796_202 bytes

// Remainder to distribute
rem = (2 * 4_194_304) % 3 = 2 bytes

// Distribute remainder across blocks using Bresenham-like algorithm
Error accumulation ensures some blocks get floor_len + 1 bytes
```

**Result**: Each block has ~2.8 MB compressible data → 3:1 ratio

### Phase 4: Thread Pool Configuration

```rust
// Detect topology
let topology = NumaTopology::detect()?;

// Configure rayon thread pool
let num_threads = match config.max_threads {
    Some(n) => n.min(num_cpus::get()),
    None => num_cpus::get(),
};

// NUMA-aware configuration (if numa_mode != Disabled)
if topology.should_enable_numa_pinning() && config.numa_mode != NumaMode::Disabled {
    // TODO: Pin threads to NUMA nodes
    // For now, just uses default rayon pool
}
```

### Phase 5: Parallel Block Generation

```rust
data.par_chunks_mut(BLOCK_SIZE)
    .enumerate()
    .for_each(|(block_idx, chunk)| {
        // Map to unique block (round-robin for dedup)
        let unique_idx = block_idx % unique_blocks;
        
        // Generate this block
        fill_block(chunk, unique_idx, copy_lens[unique_idx], call_entropy);
    });
```

**Parallelism**: Each block generated independently on separate core

### Phase 6: Block Filling

```rust
fn fill_block(out: &mut [u8], unique_idx: usize, copy_len: usize, entropy: u64) {
    // Step 1: Unique seed per block
    let seed = entropy ^ (unique_idx as u64 * PRIME);
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    
    // Step 2: Fill with high-entropy keystream
    rng.fill_bytes(out);  // ~15 GB/s per core
    
    // Step 3: Add local back-references for compression
    while made < copy_len {
        // Choose run length: 64-256 bytes
        let run_len = rng.random_range(64..=256);
        
        // Choose destination in block
        let dst = rng.random_range(0..block_len);
        
        // Choose back-reference (1-1024 bytes back)
        let back = rng.random_range(1..=1024);
        let src = dst - back;
        
        // Copy (creates compressibility)
        out.copy_within(src..src+run_len, dst);
        
        made += run_len;
    }
}
```

**Key Points**:
- High-entropy baseline → compress=1 is truly incompressible
- Back-references stay **within block** → no cross-block compression
- Run lengths and positions randomized → realistic compressor behavior

## NUMA Architecture

### UMA vs NUMA Detection

```
┌─────────────────────────────────────────────────────────────┐
│                    System Architecture                       │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
                  ┌──────────────┐
                  │ Detect Nodes │
                  └──────┬───────┘
                         │
           ┌─────────────┴─────────────┐
           ▼                           ▼
    ┌─────────────┐            ┌─────────────┐
    │ num_nodes=1 │            │ num_nodes≥2 │
    │   (UMA)     │            │   (NUMA)    │
    └─────────────┘            └─────────────┘
           │                           │
           ▼                           ▼
    Single memory          Multiple memory domains
    Single CPU socket      Multiple CPU sockets
    Cloud VM typical       Bare metal typical
    No optimization        NUMA optimization
    
Examples:
  UMA: AWS c5.large, laptop, workstation
  NUMA: 2-socket EPYC, dual Xeon, large Azure VMs
```

### NUMA Optimization Modes

```rust
pub enum NumaMode {
    /// Auto-detect: enable NUMA optimizations only on multi-node systems
    Auto,
    
    /// Force NUMA: enable optimizations even on UMA (for testing)
    Force,
    
    /// Disable: never use NUMA optimizations
    Disabled,
}
```

### Memory Locality

```
NUMA Node 0              NUMA Node 1
┌─────────────┐         ┌─────────────┐
│  CPU 0-15   │         │  CPU 16-31  │
│  (cores)    │         │  (cores)    │
├─────────────┤         ├─────────────┤
│  Memory     │         │  Memory     │
│  128 GB     │         │  128 GB     │
│  (local)    │         │  (local)    │
└─────────────┘         └─────────────┘
       │                       │
       └───────────┬───────────┘
                   │
            Cross-node bus
            (higher latency)

Best Practice:
- Allocate memory on same node as CPU
- Pin threads to specific NUMA nodes
- Avoid cross-node memory access

Current Status:
- ✅ Detection implemented
- ⚠️  Memory pinning: not implemented (uses OS default)
- ⚠️  Thread pinning: not implemented (uses rayon default)
```

## Thread Pool Management

### Current Implementation

```rust
// Uses global rayon thread pool
data.par_chunks_mut(BLOCK_SIZE)
    .enumerate()
    .for_each(|(i, chunk)| {
        // rayon schedules on available threads
        fill_block(chunk, ...);
    });
```

### Future: Custom Thread Pool

```rust
// Build custom pool with CPU affinity
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(config.max_threads.unwrap_or(num_cpus::get()))
    .spawn_handler(|thread| {
        let cpu_id = assign_cpu(thread.index());
        set_thread_affinity(cpu_id)?;
        thread.run()
    })
    .build()?;

// Use custom pool
pool.install(|| {
    data.par_chunks_mut(BLOCK_SIZE)
        .enumerate()
        .for_each(|(i, chunk)| { ... });
});
```

## Performance Characteristics

### Throughput by Configuration

| Config | compress=1 | compress=2 | compress=3 | compress=5 |
|--------|-----------|-----------|-----------|-----------|
| 1 core | 12 GB/s | 6 GB/s | 4 GB/s | 2.5 GB/s |
| 8 cores | 90 GB/s | 45 GB/s | 30 GB/s | 18 GB/s |
| 32 cores | 350 GB/s | 170 GB/s | 110 GB/s | 65 GB/s |

**Note**: Higher compression = more back-reference copies = lower throughput

### Memory Usage

```
Single-pass generation:
  Memory = total_size + overhead
         = size + (unique_blocks * sizeof(copy_lens))
         
Example (100 MB, dedup=2):
  Memory = 100 MB + (12 blocks * 8 bytes)
         ≈ 100 MB
         
Streaming generation:
  Memory = BLOCK_SIZE + chunk_size
         = 4 MB + 4 MB
         = 8 MB (constant)
```

### CPU Scaling

```
Efficiency = actual_throughput / (num_cores * single_core_throughput)

Ideal (100%): Linear scaling
Typical: 95-98% (rayon overhead, memory bandwidth)
Poor (<90%): NUMA cross-traffic, memory bottleneck
```

## Streaming Generator Internals

### Problem: Block-Level Generation

The streaming generator has a fundamental inefficiency:

```rust
// User requests 8 KiB chunk
let mut chunk = vec![0u8; 8192];
gen.fill_chunk(&mut chunk);

// Internally generates full 4 MiB block!
let mut block = vec![0u8; 4_194_304];  // Expensive!
fill_block(&mut block, ...);

// Copies only 8 KiB, discards 4 MB - 8 KiB
chunk.copy_from_slice(&block[0..8192]);

// Overhead = 4 MB / 8 KiB = 512x
```

### Solution Options

**Option 1**: Cache blocks (memory trade-off)
```rust
struct DataGenerator {
    block_cache: Option<Vec<u8>>,  // Keep last block
    last_block_idx: usize,
}
```

**Option 2**: On-demand generation (complexity trade-off)
```rust
// Generate only requested bytes, not full blocks
// Requires tracking RNG state across calls
```

**Option 3**: Force large chunks (current approach)
```rust
// Document minimum efficient chunk size
// Warn users about small chunks
```

### Current Recommendation

Use chunk_size >= BLOCK_SIZE (4 MiB) for efficiency:

```python
# Good: 5 iterations for 20 MiB
gen = Generator(size=20_000_000)
chunk = bytearray(4 * 1024 * 1024)

# Bad: 20,480 iterations for 20 MiB
chunk = bytearray(1024)
```

## Zero-Copy Python Integration

### Buffer Protocol Flow

```
Python                     Rust
┌────────────┐            ┌──────────────┐
│ bytearray  │            │              │
│ or numpy   │            │  generate()  │
│            │            │              │
└─────┬──────┘            └──────┬───────┘
      │                          │
      ▼                          ▼
┌────────────┐            ┌──────────────┐
│ Buffer     │   PyO3     │  Vec<u8>     │
│ Protocol   │◄──────────►│              │
└────────────┘            └──────────────┘
      │                          │
      │  Zero-copy write         │
      │  (unsafe ptr copy)       │
      │◄─────────────────────────┘
      ▼
┌────────────┐
│ Updated    │
│ buffer     │
└────────────┘
```

### Memory Safety

PyO3 ensures safety through:
1. **Buffer protocol validation**: Check writable, contiguous
2. **Lifetime management**: Buffer borrowed during write
3. **GIL protection**: Python GIL held during Rust execution
4. **Bounds checking**: Size validation before copy

```rust
// Safety checks
if buf.readonly() {
    return Err("Buffer must be writable");
}
if !buf.is_c_contiguous() {
    return Err("Buffer must be contiguous");
}

// Safe write
unsafe {
    std::ptr::copy_nonoverlapping(
        data.as_ptr(),
        buf.buf_ptr() as *mut u8,
        size
    );
}
```

## Entropy Sources

### Per-Call Entropy

```rust
// High-quality entropy from two sources
let time_entropy = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .as_nanos() as u64;

let urandom_entropy = rand::rng().next_u64();  // /dev/urandom

let call_entropy = time_entropy ^ urandom_entropy;
```

**Why both**:
- Time: Ensures uniqueness across calls
- Urandom: Ensures uniqueness across nodes with synchronized clocks

### Per-Block Seeding

```rust
// Each unique block gets unique seed
let seed = call_entropy ^ (unique_idx * PRIME);
let rng = Xoshiro256PlusPlus::seed_from_u64(seed);
```

**Properties**:
- Same unique_idx + call_entropy → same block (for dedup)
- Different unique_idx → different block
- Different call_entropy → all blocks different

## Future Enhancements

### 1. NUMA Thread Pinning
```rust
// Pin rayon threads to NUMA nodes
for (thread_id, numa_node) in thread_to_node_mapping {
    set_thread_affinity(thread_id, numa_node.cpus);
}
```

### 2. Block Caching (Streaming)
```rust
struct DataGenerator {
    block_cache: LruCache<usize, Vec<u8>>,  // Cache recent blocks
}
```

### 3. SIMD Acceleration
```rust
// Use SIMD for memcpy and RNG
#[cfg(target_feature = "avx2")]
unsafe fn fill_block_simd(...) { ... }
```

### 4. Async API
```rust
pub async fn generate_data_async(config: GeneratorConfig) -> Vec<u8> {
    tokio::task::spawn_blocking(|| generate_data(config)).await
}
```

### 5. Compression Validation
```rust
// Verify actual compression ratio
pub fn validate_compression(data: &[u8]) -> f64 {
    let compressed = zstd::compress(data, 3);
    data.len() as f64 / compressed.len() as f64
}
```

## References

- **Xoshiro256++**: [Paper](https://prng.di.unimi.it/)
- **Rayon**: [Docs](https://docs.rs/rayon/)
- **PyO3**: [Guide](https://pyo3.rs/)
- **NUMA**: [Linux kernel docs](https://www.kernel.org/doc/html/latest/vm/numa.html)
