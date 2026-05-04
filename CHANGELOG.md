# Changelog

All notable changes to dgen-rs/dgen-py will be documented in this file.

## [0.2.4] - 2026-05-04

### Added

#### `xor_stream` module — `UniqueXorStream`: fast, dedup-safe data generation without Rayon

New public module `dgen_data::xor_stream` exposing `UniqueXorStream`, a thread-safe data
generator that produces unique, dedup-safe byte streams at ~15 GB/s per core without
spawning any Rayon threads.

**Design**: A 1 MiB base buffer is filled with Xoshiro256++ output at construction time.
Each `fill()` call increments an `AtomicU64` counter, derives a unique Xoshiro256++ seed
via splitmix64, generates a keystream, and XORs it against the cyclic 1 MiB base to
produce the output.  The result is unique per call but requires zero synchronisation
beyond a single 64-bit atomic increment — no mutex, no allocation beyond the output
buffer.

```rust
use dgen_data::UniqueXorStream;

let stream = UniqueXorStream::new();    // 1 MiB base buffer generated once
let mut buf = vec![0u8; 8 * 1024 * 1024];

// Each call produces a different, dedup-safe 8 MiB payload
stream.fill(&mut buf);
stream.fill(&mut buf);   // completely different bytes
```

**Dedup safety**: No two 512-byte blocks across any two `fill()` calls share a
fingerprint — validated by a 100,000-block collision test in the unit suite (5 tests,
all pass).

**Thread safety**: `UniqueXorStream` is `Sync`; multiple threads may call `fill()` on
the same instance concurrently.  The `AtomicU64` counter guarantees each call receives a
unique seed regardless of interleaving.

**Constants**: `dgen_data::xor_stream::XOR_BASE_SIZE = 1024 * 1024` (1 MiB base).

**When to use**: Prefer `UniqueXorStream` over the Rayon-based `generate_data()` path
when generating large objects at high concurrency (≥ 32 concurrent writers).  At those
concurrency levels, Rayon's global pool delivers the same throughput per object, but
`UniqueXorStream` avoids all thread scheduling overhead and scales linearly.

#### `thread_local` module — Canonical thread-local pool API for async servers

New public module `dgen_data::thread_local` that canonicalises the `thread_local! { RefCell<RollingPool> }` pattern required by every async HTTP server that needs fake GET data.

Before (boilerplate in every project):
```rust
use std::cell::RefCell;
use dgen_data::RollingPool;
thread_local! {
    static POOL: RefCell<RollingPool> = RefCell::new(RollingPool::new(1, 1));
}
fn get_bytes(size: usize) -> bytes::Bytes {
    POOL.with(|p| p.borrow_mut().next_slice(size))
}
```

After (no boilerplate):
```rust
use dgen_data::thread_local::next_slice;
fn get_bytes(size: usize) -> bytes::Bytes { next_slice(size) }
```

Public functions:
- `next_slice(size) -> Bytes` — zero-copy for `size ≤ BLOCK_SIZE`, fresh Rayon generation for larger
- `reconfigure(dedup, compress)` — change data characteristics; no-op if unchanged
- `remaining() -> usize` — bytes left in current buffer before next refill (diagnostics)

**Send-safety**: The `RefCell` borrow is acquired and released within a single synchronous expression before any `.await` point, so futures that call `next_slice` are `Send` and work correctly on Tokio's multi-thread runtime.

**Continuity guarantee**: The rolling pointer advances continuously across all requests on a thread. Consecutive GETs naturally receive distinct byte ranges without any re-seeding.

#### `BLOCK_SIZE` re-exported at crate root

`dgen_data::BLOCK_SIZE` is now re-exported directly at the crate root for convenience.
Previously it was only accessible as `dgen_data::constants::BLOCK_SIZE`.

```rust
use dgen_data::BLOCK_SIZE;  // now works
```

This allows callers to write compile-time assertions and chunk-size comparisons without
importing the constants module:

```rust
const CHUNK: usize = 256 * 1024;
const _: () = assert!(CHUNK <= dgen_data::BLOCK_SIZE);
```

### Fixed

#### `generate_data()` — Rayon global pool (no per-call thread pool creation)

`generate_data()` in `src/generator.rs` previously called
`rayon::ThreadPoolBuilder::new().num_threads(N).build()` on **every invocation**, spinning
up N OS threads and tearing them down after generating a single object.  At high
concurrency (c=32, c=64) this created hundreds of simultaneous OS thread
create/destroy cycles, collapsing throughput by ~24%.

**Fix**: The non-NUMA path now calls `par_chunks_mut().enumerate().for_each()` directly
on the data slice.  Rayon dispatches work to its **global** thread pool, which is
constructed once at first use and reused for the lifetime of the process.  The NUMA-aware
path (feature `thread-pinning`) is unchanged — it still builds a topology-pinned custom
pool.

**Impact**: With the global pool, sai3-bench `fresh` mode throughput at c=32 improved
from ~1,040 MiB/s to ~1,987 MiB/s (8 MiB objects, loopback, loki-russ 28-core Xeon).
The previous −24% concurrency cliff is eliminated.

---

## [0.2.3] - 2026-04-15

### Added

#### `RollingPool` — Zero-Copy Rolling Buffer (Rust API)

New public type `dgen_data::RollingPool` that generates a single 1 MB block once, then
serves successive calls as zero-copy `Bytes::slice()` windows.  The pool refills
automatically when exhausted or when the dedup/compress parameters change.

```rust
use dgen_data::RollingPool;

let mut pool = RollingPool::new(1, 1);          // dedup=1, compress=1

// Zero-copy for objects ≤ 1 MB: no allocation
let slice = pool.next_slice(64 * 1024);         // 64 KB window, Arc increment only

// Objects > 1 MB: fresh buffer (large-object bypass)
let big  = pool.next_slice(4 * 1024 * 1024);   // 4 MB, generates fresh
```

- `new(dedup, compress)` — creates pool and generates the first 1 MB block
- `next_slice(size)` — zero-copy for `size ≤ 1 MB`; fresh for larger
- `reconfigure(dedup, compress)` — no-op if unchanged; forces refill if either changes
- `dedup()`, `compress()`, `remaining()` — getters
- In-flight `Bytes` slices remain valid after a pool refill (Arc-shared backing)

#### Automatic Rolling Pool Fast Path in `generate_buffer()` (Python API)

The Python `generate_buffer(size, ...)` function now automatically uses the rolling pool
for objects smaller than 1 MB without NUMA pinning.  No API change needed — existing
code gets the speedup transparently.

Before (v0.2.2): every `generate_buffer(64*1024)` call generated a fresh 1 MB block
and discarded 15/16 of it (16× allocation waste).

After (v0.2.3): the thread-local rolling pool serves 16,384 consecutive 64 KB calls
from a single 1 MB generation, refilling only when exhausted.

```python
import dgen_py

# Unchanged call — automatically benefits from rolling pool for size < 1 MB
buf = dgen_py.generate_buffer(64 * 1024)
```

#### `BufferPool` Python Class (Explicit Pool API)

New Python class for workloads that want explicit control over pool lifecycle,
or that need to share one pool across many calls in tight loops:

```python
import dgen_py

pool = dgen_py.BufferPool(dedup_ratio=1.0, compress_ratio=1.0)

# 10 000 × 64 KB slices — zero-copy from one 1 MB backing buffer + refills
images = [pool.next_slice(64 * 1024) for _ in range(10_000)]
print(f"Generated {sum(len(img) for img in images) / 1e6:.1f} MB")  # → 640.0 MB

# Change data pattern mid-stream (forces one refill, then zero-copy resumes)
pool.reconfigure(dedup_ratio=2.0, compress_ratio=4.0)

print(pool.remaining)      # bytes left before next refill
print(pool.dedup_ratio)    # 2
print(pool.compress_ratio) # 4
```

### Performance

Benchmark: 1 GB total output per scenario, release build, single thread.

```
── BASELINE: generate_data_simple() ────────────────────────────────────────

Strategy                     Obj size       Calls      Output  Throughput   Alloc×
----------------------------------------------------------------------------------
generate_data_simple            64 KB       16384        1 GB    107 MB/s     16.0x
generate_data_simple             1 MB        1024        1 GB   1.78 GB/s      1.0x
generate_data_simple             1 GB           1        1 GB   9.73 GB/s      1.0x
----------------------------------------------------------------------------------

── ROLLING POOL: RollingPool::next_slice() ──────────────────────────────────

Strategy                     Obj size       Calls      Output  Throughput   Alloc×
----------------------------------------------------------------------------------
RollingPool::next_slice         64 KB       16384        1 GB   1.73 GB/s      1.0x
RollingPool::next_slice          1 MB        1024        1 GB   1.74 GB/s      1.0x
RollingPool::next_slice          1 GB           1        1 GB   9.49 GB/s      0.0x
----------------------------------------------------------------------------------

── IMPROVEMENT SUMMARY ──────────────────────────────────────────────────────
Object size             Baseline         RollingPool     Speedup
------------------------------------------------------------------
64 KB                   107 MB/s           1.73 GB/s      16.19x
1 MB                   1.78 GB/s           1.74 GB/s       0.98x
1 GB                   9.73 GB/s           9.49 GB/s       0.98x
------------------------------------------------------------------
```

**Key design properties:**
- 64 KB objects: **16× throughput improvement** — eliminates the 1 MB minimum-allocation waste
- 1 MB objects: **unchanged** — exact BLOCK_SIZE fit, one refill per call (same as before)
- 1 GB objects: **unchanged** — large-object bypass path, generation cost dominates
- Zero regression for any object size ≥ 1 MB

## [0.2.2] - 2026-03-27

### Changed
- Default wheel builds now ship without NUMA/hwloc support to improve compatibility across Linux environments.
- NUMA support is now documented as an opt-in source build path using Cargo features.
- Python support policy updated to Python 3.11+; Python 3.10 is no longer supported.

## [0.2.1] - 2026-03-26

### Changed
- Wheel builds now explicitly target Python 3.10+. The `requires-python = ">=3.10"` constraint is enforced in `pyproject.toml`. Build separate wheels per Python version with `maturin build --release -i python3.10` and `maturin build --release -i python3.11`.

## [0.1.7] - 2026-01-25

### Added
- **Stream Reset via `set_seed()` Method**: Dynamically change the random seed during generation to create reproducible data patterns
  - Calling `set_seed(seed)` resets the data stream to the beginning of that seed's sequence
  - Functionally equivalent to creating a new generator with minimal overhead
  - Enables complex patterns: headers/payloads/footers with different seeds, striped patterns for RAID testing
  - Available in both Rust (`DataGenerator::set_seed()`) and Python (`Generator.set_seed()`)
  - Comprehensive test coverage: `test_set_seed_stream_reset()` in Rust, `test_set_seed_method.py` in Python

### Changed
- Internal RNG architecture now uses block sequence counter for deterministic stream reset
- Block-level RNG derivation ensures same seed always produces identical data stream
## [0.1.6] - 2026-01-23

### Added

#### Reproducible Data Generation
- **NEW: `seed` parameter** for `Generator()` constructor
  - Optional `u64` seed value for reproducible random data streams
  - When `seed=None` (default): Uses time + urandom entropy (non-deterministic, backward compatible)
  - When `seed=Some(value)`: Generates identical data for same configuration (reproducible testing)
  - Enables reproducible benchmarking, testing, and debugging workflows

#### Python API Enhancement
```python
# Reproducible mode - same seed produces identical data
gen1 = dgen_py.Generator(size=10*1024**3, seed=12345)
gen2 = dgen_py.Generator(size=10*1024**3, seed=12345)
# gen1 and gen2 will produce identical data

# Non-deterministic mode (default) - different data each time
gen3 = dgen_py.Generator(size=10*1024**3)  # seed=None (default)
gen4 = dgen_py.Generator(size=10*1024**3)  # seed=None (default)
# gen3 and gen4 will produce different data
```

#### Testing
- **NEW: `python/examples/test_seed_reproducibility.py`**: Comprehensive test suite
  - Validates reproducibility with same seed
  - Confirms non-determinism without seed
  - Verifies different seeds produce different data
  - SHA256 hash comparison for data integrity

### Changed

#### Core Implementation
- Modified `GeneratorConfig` struct to include optional `seed` field
- Updated `DataGenerator::new()` to use provided seed or generate entropy
- Enhanced `generate_call_entropy()` usage to be conditional on seed parameter

#### Code Quality
- Fixed all `clippy::identity_op` warnings in constants
- Added `#[allow(clippy::too_many_arguments)]` for PyO3 constructor (API requirement)
- Updated all examples and benchmarks to include new `seed` field

### Backward Compatibility
- **Fully backward compatible**: `seed` defaults to `None`
- Existing code continues to work without modification
- Default behavior unchanged (time + urandom entropy)

---

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
