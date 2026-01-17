# Changelog

All notable changes to dgen-rs/dgen-py will be documented in this file.

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
