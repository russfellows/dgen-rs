# Cache Optimization Strategy for Parallel Streaming

**Date**: January 17, 2026  
**Context**: 384-core NUMA system with 16 nodes

## The Cache vs Parallelism Tradeoff

### Problem

We have two competing goals:
1. **Cache efficiency**: 4 MB blocks fit in L3 cache (~40-60 MB per core)
2. **Parallelization**: Need large chunks to trigger parallel generation (≥ 8 MB)

### Solution: Two-Level Architecture

The implementation uses a **two-level approach**:

```
User Chunk (64-256 MB)
  └─> Parallelized across threads
       └─> Each thread generates 4 MB blocks (cache-friendly)
```

## Optimal Chunk Sizes by Use Case

| Chunk Size | Blocks | Parallelism | Cache Efficiency | Best For |
|-----------|--------|-------------|------------------|----------|
| 4 MB | 1 | ❌ Sequential | ✅ Perfect (fits L3) | Single-core only |
| 8 MB | 2 | ⚠️ Minimal (2 threads) | ✅ Perfect | Low parallelism |
| 16 MB | 4 | ⚠️ Low (4 threads) | ✅ Perfect | Small systems |
| 32 MB | 8 | ✅ Moderate (8 threads) | ✅ Perfect | Balanced |
| **64 MB** | **16** | **✅ Good (16 threads)** | **✅ Perfect** | **RECOMMENDED** |
| 128 MB | 32 | ✅ Good (32 threads) | ✅ Perfect | High parallelism |
| 256 MB | 64 | ✅ Excellent (64 threads) | ✅ Perfect | Maximum parallelism |
| 512 MB | 128 | ✅ Excellent (128 threads) | ⚠️ May thrash cache | Very high parallelism |

### Why Each Thread Still Uses 4 MB Blocks

Even with large chunks, **each individual thread generates 4 MB blocks**:

```rust
// Parallel path in fill_chunk_parallel()
(0..num_blocks)
    .into_par_iter()  // Rayon distributes blocks across threads
    .map(|i| {
        // Each thread generates ONE 4 MB block at a time
        let mut block_buf = vec![0u8; BLOCK_SIZE];  // 4 MB
        fill_block(&mut block_buf, ...);
        block_buf
    })
```

**Result**: Each thread's working set = 4 MB → Fits in L3 cache!

## Recommended Configuration for 384-Core System

### Option 1: Balanced (RECOMMENDED)
```python
gen = dgen_py.Generator(
    size=total_size,
    numa_mode="auto",
    max_threads=None  # All 384 cores
)
buffer = bytearray(64 * 1024**2)  # 64 MB chunks

# Performance: 50-80 GB/s
# Memory: Only 64 MB at a time
# Cache: Perfect (each thread works on 4 MB)
```

### Option 2: Maximum Parallelism
```python
buffer = bytearray(256 * 1024**2)  # 256 MB chunks

# Performance: 80-120 GB/s (higher peak)
# Memory: 256 MB at a time
# Cache: Still good (each thread = 4 MB)
# Tradeoff: More threads = more context switching
```

### Option 3: Conservative (Many Streams)
```python
buffer = bytearray(32 * 1024**2)  # 32 MB chunks

# Performance: 40-60 GB/s
# Memory: Only 32 MB at a time
# Use when: Running many parallel streams
```

## Why NOT Use 4 MB Chunks?

With the new parallel implementation:

```python
# ❌ BAD: 4 MB chunks
buffer = bytearray(4 * 1024**2)
gen.fill_chunk(buffer)
# Result: Sequential generation (1 block < 2 block threshold)
# Performance: ~4 GB/s (only 1 of 384 cores used!)
```

**Problem**: 4 MB = 1 block → Falls below 2-block threshold → Sequential path

## Cache Architecture on HBv5 (AMD EPYC Milan)

```
Per-Core:
  L1i: 32 KB
  L1d: 32 KB
  L2: 512 KB
  L3: 32 MB (shared per CCX - ~4 cores)

384 cores total = ~96 CCX groups
Each CCX: 4 cores sharing 32 MB L3
```

**Why 4 MB blocks are perfect**:
- 4 MB × 4 cores = 16 MB working set
- 16 MB < 32 MB L3 cache
- Still room for other data structures

## Memory Bandwidth Considerations

### Single-Socket EPYC (64 cores)
- Memory bandwidth: ~200 GB/s
- Peak generation: ~120 GB/s (memory-bound)
- **Optimal chunk**: 64-128 MB

### Dual-Socket EPYC (128 cores)  
- Memory bandwidth: ~400 GB/s
- Peak generation: ~250 GB/s (memory-bound)
- **Optimal chunk**: 128-256 MB

### Your System (384 cores, 16 NUMA nodes)
- Memory bandwidth: ~800-1000 GB/s (estimated)
- Peak generation: ~500-800 GB/s (memory-bound)
- **Optimal chunk**: 256 MB for max throughput
- **Recommended chunk**: 64 MB for balance

## Thread Pool Overhead

Creating rayon thread pools has overhead:

| Chunk Size | Thread Pool Creates/Second | Overhead |
|-----------|---------------------------|----------|
| 4 MB | 262,144/s (for 1 TB/s) | ❌ Massive |
| 64 MB | 16,384/s (for 1 TB/s) | ⚠️ Noticeable |
| 256 MB | 4,096/s (for 1 TB/s) | ✅ Acceptable |

**Future optimization**: Reuse thread pool across calls (performance improvement ~10-20%)

## Practical Benchmarking

Test different chunk sizes on your system:

```python
import dgen_py
import time

CHUNK_SIZES = [
    (8 * 1024**2, "8 MB"),
    (16 * 1024**2, "16 MB"),  
    (32 * 1024**2, "32 MB"),
    (64 * 1024**2, "64 MB"),
    (128 * 1024**2, "128 MB"),
    (256 * 1024**2, "256 MB"),
]

for chunk_size, label in CHUNK_SIZES:
    gen = dgen_py.Generator(
        size=10 * 1024**3,  # 10 GB
        numa_mode="auto"
    )
    buffer = bytearray(chunk_size)
    
    start = time.perf_counter()
    while not gen.is_complete():
        gen.fill_chunk(buffer)
    elapsed = time.perf_counter() - start
    
    throughput = (10 * 1024**3) / elapsed / (1024**3)
    print(f"{label:10} {throughput:6.2f} GB/s")
```

## Expected Results on 384-Core System

```
Chunk Size  Throughput   Analysis
8 MB        15-25 GB/s   Too small - limited parallelism
16 MB       30-45 GB/s   Better but still limited
32 MB       50-70 GB/s   Good parallelism starting
64 MB       70-100 GB/s  Sweet spot - RECOMMENDED
128 MB      90-120 GB/s  Near peak
256 MB      100-140 GB/s Maximum (may have diminishing returns)
```

## Final Recommendation

**For 384-core HBv5 system**:

```python
# RECOMMENDED: 64 MB chunks
buffer = bytearray(64 * 1024 * 1024)

# Rationale:
# ✅ 16 blocks → Good parallelization (up to 384 threads)
# ✅ Each thread works on 4 MB → Perfect cache fit
# ✅ Reasonable memory footprint (64 MB)
# ✅ Low thread pool overhead
# ✅ Expected: 70-100 GB/s
```

**For maximum throughput** (if memory allows):

```python
# AGGRESSIVE: 256 MB chunks  
buffer = bytearray(256 * 1024 * 1024)

# Rationale:
# ✅ 64 blocks → Excellent parallelization
# ✅ Still cache-friendly per thread (4 MB)
# ⚠️ Higher memory usage (256 MB)
# ✅ Expected: 100-140 GB/s
```

---

**Summary**: Use **64 MB chunks** for best balance of cache efficiency, parallelization, and memory usage on large NUMA systems.
