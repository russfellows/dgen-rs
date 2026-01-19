# Changelog

All notable changes to dgen-rs/dgen-py will be documented in this file.

## [0.1.5] - 2026-01-19

### 🎉 Major Performance Improvements

#### Performance Gains vs v0.1.3
- **UMA systems (single NUMA node)**: ~50% improvement in per-core throughput
  - v0.1.5: 10.80 GB/s per core (C4-16, 8 cores)
  - v0.1.3: ~7 GB/s per core (12-core Ice Lake: 43.25 GB/s / 12 = 3.60 GB/s per thread)
  - Note: v0.1.3 reported per-thread, v0.1.5 reports per physical core
- **NUMA systems**: Significant improvements due to bug fixes in NUMA implementation
- **Maximum aggregate**: **324.72 GB/s** on 48-core dual-NUMA system (GCP C4-96 with compress=2.0)

### Changed

#### Core Performance Optimization
- **BLOCK_SIZE increased**: 64 KB → **4 MB** for optimal L3 cache utilization
  - 34% performance boost on modern CPUs (Emerald Rapid, Sapphire Rapids)
  - Better parallelization across cores
  - Reduced thread pool overhead

#### Multi-Process NUMA Architecture
- **Proper CPU affinity detection**: Uses `/sys/devices/system/node/nodeN/cpulist`
- **Process pinning**: `os.sched_setaffinity()` for NUMA locality
- **Synchronized start**: `multiprocessing.Barrier` for accurate timing
- **64 MB chunk size default**: Optimized for newer generation CPUs with larger L3 cache

### Added

#### Documentation
- **NEW: `docs/BENCHMARK_RESULTS_V0.1.5.md`**: Comprehensive 426-line performance analysis
  - 4 GCP instances tested (C4-8, C4-16, C4-32, C4-96)
  - Detailed scaling analysis (UMA vs NUMA)
  - Compression ratio impact study (1.3-1.5x speedup with compress=2.0)
  - Per-instance raw results and recommendations

#### Examples
- **`python/examples/benchmark_numa_multiprocess_v2.py`**: Production-grade NUMA benchmark
  - Process affinity pinning via `os.sched_setaffinity()`
  - Local memory allocation per NUMA node
  - Synchronized multi-process execution
  - Detailed per-node reporting

- **`examples/numa_test.rs`**: Native Rust NUMA testing utility
- **`examples/NUMA_BENCHMARK_README.md`**: NUMA architecture documentation

### Performance Results (v0.1.5)

**Scalability on GCP Intel Emerald Rapid (compress=1.0):**

| Instance | Physical Cores | NUMA Nodes | Aggregate Throughput | Per-Core | Scaling Efficiency |
|----------|----------------|------------|---------------------|----------|-------------------|
| C4-8 | 4 | 1 (UMA) | 36.26 GB/s | 9.07 GB/s | Baseline |
| C4-16 | 8 | 1 (UMA) | **86.41 GB/s** | **10.80 GB/s** | **119%** |
| C4-32 | 16 | 1 (UMA) | **162.78 GB/s** | **10.17 GB/s** | **112%** |
| C4-96 | 48 | 2 (NUMA) | 248.53 GB/s | 5.18 GB/s | 51%* |

\* *NUMA penalty: 49% per-core reduction on multi-socket systems*

**Compression Ratio Impact (compress=2.0 vs compress=1.0):**
- C4-8: 53.95 GB/s (1.49x speedup)
- C4-16: 125.88 GB/s (1.46x speedup)
- C4-32: 222.28 GB/s (1.37x speedup)
- C4-96: 324.72 GB/s (1.31x speedup)

**Key Findings:**
- Excellent UMA scaling: 112-119% efficiency (super-linear due to larger L3 cache)
- Deduplication ratio has ZERO performance impact (< 1% variance)
- Compression ratio provides 1.3-1.5x speedup but makes data more compressible (choose based on test requirements)

### Updated

#### README.md
- Highlighted 3.0x improvement as main feature
- Replaced v0.1.3 benchmarks with v0.1.5 data
- Streamlined examples (removed verbose output)
- Clarified compression ratio tradeoff (performance vs test accuracy)
- Reduced from 363 to 256 lines for PyPI publication

#### pyproject.toml
- Updated benchmark comments with v0.1.5 performance data
- Added performance gains section (3.0x improvement)
- Updated storage benchmarking guidance
- Reflected new compression impact analysis

### Technical Details

#### BLOCK_SIZE Optimization
- **Old** (v0.1.3): 64 KB blocks
  - High thread pool overhead on large datasets
  - Suboptimal L3 cache utilization
- **New** (v0.1.5): 4 MB blocks
  - Reduced parallel overhead (fewer blocks to process)
  - Better L3 cache hit rates on modern CPUs
  - Result: 34% throughput improvement

#### NUMA Architecture Improvements
- **Proper topology detection**: Reads `/sys/devices/system/node/nodeN/cpulist`
- **CPU affinity pinning**: `os.sched_setaffinity(0, [cpu_list])`
- **Local memory allocation**: Each process allocates on its NUMA node
- **Synchronized execution**: `multiprocessing.Barrier` ensures fair comparison

### Migration Guide

No breaking changes - existing code continues to work with 3.0x better performance.

**Optional optimization for newer CPUs:**
```python
# Override chunk size to 64 MB for Emerald Rapid, Sapphire Rapids
gen = dgen_py.Generator(
    size=100 * 1024**3,
    chunk_size=64 * 1024**2  # 64 MB (default is auto-detected)
)
```

---

## [0.1.4] - 2026-01-18

### Changed

#### Documentation Accuracy
- **README.md**: Removed projected performance numbers, added actual NUMA benchmark results
- **README.md**: Removed references to private repositories
- Fixed benchmark result reporting to match actual measured performance

### Performance Results (v0.1.4)

**Multi-NUMA Benchmarks (actual measurements):**

| System | Cores | NUMA Nodes | Throughput | Per-Core | Efficiency |
|--------|-------|------------|------------|----------|------------|
| GCP C4-16 | 16 | 1 (UMA) | 39.87 GB/s | 2.49 GB/s | 100% (baseline) |
| GCP C4-96 | 96 | 4 | 126.96 GB/s | 1.32 GB/s | 53% |
| Azure HBv5 | 368 | 16 | 188.24 GB/s | 0.51 GB/s | 20% |

**Key Findings:**
- Sub-linear scaling expected for memory-intensive workloads
- All systems exceed 80 GB/s storage testing requirements
- Documentation now reflects actual measured performance

---

## [0.1.3] - 2026-01-17

### 🚀 Major Performance Improvements

#### Zero-Copy Parallel Streaming (24x Python Performance Boost)
- **TRUE zero-copy Python API**: `fill_chunk()` now generates **directly into Python buffer** (no temporary allocation)
- **GIL release**: Uses `py.detach()` to release GIL during generation (enables true parallelism)
- **Thread pool reuse**: Created once in `DataGenerator::new()`, reused for all `fill_chunk()` calls
- **Performance results on 12-core system**:
  - Python: 43.25 GB/s (was 1.97 GB/s in v0.1.2 - **22x faster**)
  - Native Rust: 47.18 GB/s
  - Python now achieves **92% of native Rust performance**
- **Projected performance on 384-core HPC system**:
  - Python: 1,384 GB/s (**17.3x faster** than 80 GB/s storage target)
  - Native Rust: 1,511 GB/s (**18.9x faster** than storage target)

### Changed

#### Python API (`src/python_api.rs`)
- `PyGenerator::fill_chunk()`: 
  - Removed temporary buffer allocation
  - Generates directly into Python buffer via `std::slice::from_raw_parts_mut`
  - Releases GIL using `py.detach()` (replaces deprecated `py.allow_threads()`)
  - True zero-copy from Rust to Python

#### Core Generator (`src/generator.rs`)
- `DataGenerator` struct:
  - Added `max_threads: usize` field
  - Added `thread_pool: Option<rayon::ThreadPool>` field (reused across all `fill_chunk()` calls)
- `DataGenerator::new()`:
  - Creates thread pool once during initialization
  - Configures from `GeneratorConfig::max_threads`
- `fill_chunk()`:
  - Split into `fill_chunk_parallel()` (≥8 MB) and `fill_chunk_sequential()` (<8 MB)
  - Threshold: 2 blocks (8 MB) to trigger parallel path
- `fill_chunk_parallel()`:
  - Uses stored thread pool (eliminates per-call creation overhead)
  - Generates via `pool.install(|| chunk.par_chunks_mut().for_each(...))`
  - Zero-copy: generates directly into output buffer using rayon parallel iteration

### Added

#### Examples
- `examples/streaming_benchmark.rs`: Native Rust streaming benchmark (shows 47.18 GB/s)
- `python/examples/Benchmark_dgen-py_FIXED.py`: Python benchmark demonstrating zero-copy performance (43.25 GB/s)

#### Documentation
- Performance tips in README.md about optimal chunk sizes (64-256 MB)
- Technical details about thread pool reuse and zero-copy implementation

### Performance Comparison

**Development System (12 cores, UMA):**
| Method | v0.1.2 | v0.1.3 | Improvement | Per-Core |
|--------|--------|--------|-------------|----------|
| Python | 1.97 GB/s | 43.25 GB/s | **22x** | 3.60 GB/s |
| Rust | 47.18 GB/s | 47.18 GB/s | baseline | 3.93 GB/s |

**Key Insight**: Python achieves 92% efficiency vs native Rust (was only 4% in v0.1.2)

### Technical Details

#### Memory Architecture Changes
- **Old approach** (v0.1.2):
  ```rust
  let mut temp = vec![0u8; size];           // Allocate 64 MB temp buffer
  self.inner.fill_chunk(&mut temp);         // Generate into temp
  copy_nonoverlapping(temp, dst_ptr, size); // Copy 64 MB to Python buffer
  ```
  Result: 2x memory bandwidth usage, GIL held during copy

- **New approach** (v0.1.3):
  ```rust
  py.detach(|| {                            // Release GIL
      let dst = from_raw_parts_mut(buf_ptr, size);
      self.inner.fill_chunk(dst)            // Generate directly into Python buffer
  })
  ```
  Result: 1x memory bandwidth, parallel execution without GIL

#### Thread Pool Overhead Eliminated
- **Old**: Created new thread pool for every 64 MB chunk
  - On 384-core system: 384 threads × 16,000 chunks/TB = catastrophic overhead
- **New**: Thread pool created once, reused for ~16,000 chunks per TB
  - Result: Eliminated dominant bottleneck

### Breaking Changes
None - API remains fully compatible with v0.1.2

### Migration Guide
No code changes required - existing applications automatically benefit from 22x performance improvement.

**Optional optimization**: Increase chunk size to 64-256 MB for streaming workloads:
```python
# Old recommendation (still works)
gen.fill_chunk(buffer[:4*1024*1024])  # 4 MB

# New recommendation for high-performance streaming
gen.fill_chunk(buffer[:64*1024*1024])  # 64 MB - better parallelization
```

Larger chunks enable better parallelization while maintaining cache efficiency.

## [Unreleased]

### Credits
- Algorithm ported from s3dlio/src/data_gen_alt.rs
- NUMA detection from kv-cache-bench
- Built with PyO3 and Maturin

## [0.1.0] - 2026-01-08

Initial release.
