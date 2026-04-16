# dgen-py

**The worlds fastest Python random data generation - with NUMA optimization and zero-copy interface**

[![Version](https://img.shields.io/badge/version-0.2.3-blue)](https://pypi.org/project/dgen-py/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![PyPI](https://img.shields.io/pypi/v/dgen-py)](https://pypi.org/project/dgen-py/)
[![Python Version](https://img.shields.io/badge/python-3.11+-blue.svg)](https://www.python.org)
[![Tests](https://img.shields.io/badge/tests-6%20passing-success)](https://github.com/russfellows/dgen-rs)

## Features

- 🚀 **Blazing Fast**: 10 GB/s per core, up to 300 GB/s verified
- ⚡ **Ultra-Fast Allocation**: `create_bytearrays()` for 1,280x faster pre-allocation than Python (v0.2.0)
- 🔁 **Zero-Copy Rolling Pool**: `generate_buffer()` auto-uses a rolling pool for small objects — 16× speedup for 64 KB objects with no API change (NEW in v0.2.3)
- 🏊 **Explicit Pool API**: `BufferPool` class for tight loops — pre-generate once, serve millions of slices (NEW in v0.2.3)
- 🎯 **Controllable Characteristics**: Configurable deduplication and compression ratios
- 🔄 **Reproducible Data**: Seed parameter for identical data generation (v0.1.6) with dynamic reseeding (v0.1.7)
- 🔬 **Multi-Process NUMA**: One Python process per NUMA node for maximum throughput
- 🐍 **True Zero-Copy**: Python buffer protocol with direct memory access (no data copying)
- 📦 **Streaming API**: Generate terabytes of data with constant 32 MB memory usage
- 🧵 **Thread Pool Reuse**: Created once, reused across all operations
- 🛠️ **Built with Rust**: Memory-safe, production-quality implementation

## Performance

### API Throughput by Object Size — v0.2.3 (12-vCPU VM, 31 GB RAM)

Two usage patterns are benchmarked; run `cargo run --release --example speed-table` (Rust)
or `python examples/speed_table.py` (Python) to reproduce on your hardware.

**Per-object**: One generation call per object, called repeatedly in a tight loop.
Only generation time is measured; buffer deallocation (`munmap`/`free`) happens outside
the timer. Three implementations compared:
- `≤ 1 MB` **dgen-py**: `BufferPool.next_slice()` — zero-copy slice from pre-generated block
- `> 1 MB` **dgen-py**: `Generator(size).get_chunk()` — new Rayon thread pool per call
- **NumPy**: `np.random.default_rng().random(N//8)` — PCG64 float64, rng reused, single-threaded

**Streaming (dgen-py only)**: One `Generator` for the entire run, 32 MB chunks.
Thread pool created **once** and reused for every `fill_chunk()` call. NumPy has no
equivalent streaming API.

| Object | Rust per-obj | dgen-py per-obj | NumPy per-obj | dgen-py stream |
|--------|:------------:|:---------------:|:-------------:|:--------------:|
| 64 B   |   894 MB/s   |    228 MB/s     |    73 MB/s    |   55–59 GB/s   |
| 512 B  |  1.77 GB/s   |   1.24 GB/s     |   476 MB/s    |   57–61 GB/s   |
| 4 KB   |  1.79 GB/s   |   1.84 GB/s     |  1.57 GB/s    |   59–62 GB/s   |
| 64 KB  |  1.72 GB/s   |   1.96 GB/s     |  2.36 GB/s    |   56–59 GB/s   |
| 1 MB   |  1.74 GB/s   |   2.00 GB/s     |  2.46 GB/s    |   57–61 GB/s   |
| 10 MB  |  8.04 GB/s   |   8.35 GB/s     |  2.46 GB/s    |   53–58 GB/s   |
| 100 MB | 16.52 GB/s   |  17.45 GB/s     |  1.91 GB/s    |   58–61 GB/s   |
| 1 GB   | 17.89 GB/s   |  19.75 GB/s     |  1.87 GB/s    |   52–61 GB/s   |
| 10 GB  |**19.76 GB/s**| **20.06 GB/s**  |  1.83 GB/s    | **55–63 GB/s** |

**Key observations:**
- **NumPy plateaus at ~2.5 GB/s** for objects ≥ 1 MB — PCG64 is single-threaded and
  saturates at one core's DRAM write bandwidth.  For 100 MB+ objects, the working set
  spills out of L3 cache and NumPy drops to ~1.9 GB/s.
- **dgen-py and Rust scale to 17–20 GB/s** at 100 MB+ by filling all 12 cores with
  Xoshiro256++ in parallel; at 10 GB they hit peak DRAM write bandwidth (~20 GB/s).
- **NumPy is faster than dgen-py for 64 KB–1 MB objects** (2.36–2.46 GB/s vs 1.96–2.00 GB/s):
  at those sizes the PCG64 generator fits entirely in L2/L3 cache and runs faster than
  dgen-py's per-call pool-slice overhead.  Below 64 KB, dgen-py pulls ahead via its
  zero-copy pool; above 1 MB, dgen-py's parallel Rayon threads dominate.
- **Streaming (dgen-py)** hits 52–63 GB/s by reusing the Rayon thread pool across 32 MB
  chunks; the working-set stays in L3 cache and only final writeback reaches DRAM.
- **NumPy has no streaming API** for comparison.

### Streaming Benchmark - 100 GB Test

Comparison of streaming random data generation methods on a 12-core system:

| Method | Throughput | Speedup vs Baseline | Memory Required |
|--------|------------|---------------------|-----------------|
| **os.urandom()** (baseline) | 0.34 GB/s | 1.0x | Minimal |
| **NumPy Multi-Thread** | 1.06 GB/s | 3.1x | 100 GB RAM* |
| **Numba JIT Xoshiro256++** (streaming) | 57.11 GB/s | 165.7x | 32 MB RAM |
| **dgen-py v0.1.5** (streaming) | **58.46 GB/s** | **169.6x** | **32 MB RAM** |

\* *NumPy requires full dataset in memory (10 GB tested, would need 100 GB for 100 GB dataset)*

**Key Findings:**
- **dgen-py matches Numba's streaming performance** (58.46 vs 57.11 GB/s)
- **55x faster than NumPy** while using **3,000x less memory** (32 MB vs 100 GB)
- **Streaming architecture**: Can generate unlimited data with only 32 MB RAM
- **Per-core throughput**: 4.87 GB/s (12 cores)

> **⚠️ Critical for Storage Testing**: **ONLY dgen-py** supports configurable **deduplication and compression ratios**. All other methods (os.urandom, NumPy, Numba) generate purely random data with maximum entropy, making them unsuitable for realistic storage system testing. Real-world storage workloads require controllable data characteristics to test deduplication engines, compression algorithms, and storage efficiency—capabilities unique to dgen-py.

### Multi-NUMA Scalability - GCP Emerald Rapid

**Scalability testing** on Google Cloud Platform Intel Emerald Rapid systems (1024 GB workload, compress=1.0):

| Instance | Physical Cores | NUMA Nodes | Aggregate Throughput | Per-Core | Scaling Efficiency |
|----------|----------------|------------|---------------------|----------|-------------------|
| **C4-8** | 4 | 1 (UMA) | 36.26 GB/s | 9.07 GB/s | Baseline |
| **C4-16** | 8 | 1 (UMA) | **86.41 GB/s** | **10.80 GB/s** | **119%** |
| **C4-32** | 16 | 1 (UMA) | **162.78 GB/s** | **10.17 GB/s** | **112%** |
| **C4-96** | 48 | 2 (NUMA) | 248.53 GB/s | 5.18 GB/s | 51%* |

\* *NUMA penalty: 49% per-core reduction on multi-socket systems, but still achieves highest absolute throughput*

**Key Findings:**
- **Excellent UMA scaling**: 112-119% efficiency on single-NUMA systems (super-linear due to larger L3 cache)
- **Per-core performance**: 10.80 GB/s on C4-16 (3.0x improvement vs dgen-py v0.1.3's 3.60 GB/s)
- **Compression tradeoff**: compress=2.0 provides 1.3-1.5x speedup, but makes data compressible (choose based on your test requirements, not performance)
- **Storage headroom**: Even modest 8-core systems exceed 86 GB/s (far beyond typical storage requirements)

**See [docs/BENCHMARK_RESULTS_V0.1.5.md](docs/BENCHMARK_RESULTS_V0.1.5.md) for complete analysis**

## Installation

### From PyPI (Recommended)

```bash
pip install dgen-py
```

Default PyPI wheels are built without NUMA/hwloc support so they remain broadly compatible across Linux distributions.

### Python Version Support

- Supported: Python 3.11+
- Not supported: Python 3.10 and older

### Enable NUMA Support (Source Build)

NUMA-aware topology and NUMA-local allocation require building from source with the `numa` feature.

```bash
# System deps (Linux)
# Ubuntu/Debian:
sudo apt-get install libudev-dev libhwloc-dev

# RHEL/CentOS/Fedora:
sudo yum install systemd-devel hwloc-devel

# Build from source with NUMA enabled
pip install --no-binary dgen-py dgen-py \
    --config-settings=build-args="--features python-bindings,numa,thread-pinning"
```

### System Requirements

**For source builds with NUMA support (Linux only):**
```bash
# Ubuntu/Debian
sudo apt-get install libudev-dev libhwloc-dev

# RHEL/CentOS/Fedora
sudo yum install systemd-devel hwloc-devel
```

**Note**: Without NUMA/hwloc, dgen-py still delivers high performance on UMA and single-node cloud systems. The limitation is on true multi-NUMA systems where NUMA-local memory placement and topology-aware optimization are not available.

## Quick Start

### Version 0.2.3: Rolling Pool for Small-Object Workloads 🎉

dgen-py v0.2.2 had a hidden inefficiency: `generate_buffer(size)` always generated **at least 1 MB** internally (the dgen-data block size floor), even when you only asked for 64 KB. That meant 15/16 of every allocation was generated and thrown away — a **16× waste**.

v0.2.3 eliminates this with a **rolling buffer pool**. A single 1 MB block is generated, then served as zero-copy slices. Refills happen only on exhaustion or config change.

#### Benchmark results (1 GB total output per scenario, release build):

```
── BASELINE (v0.2.2): generate_data_simple() ───────────────────────────────

Strategy                     Obj size       Calls      Output  Throughput   Alloc×
----------------------------------------------------------------------------------
generate_data_simple            64 KB       16384        1 GB    107 MB/s     16.0x
generate_data_simple             1 MB        1024        1 GB   1.78 GB/s      1.0x
generate_data_simple             1 GB           1        1 GB   9.73 GB/s      1.0x
----------------------------------------------------------------------------------

── ROLLING POOL (v0.2.3): RollingPool::next_slice() ─────────────────────────

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
- 1 MB objects: **unchanged** — exact pool block size, same generation cost as before
- 1 GB objects: **unchanged** — large-object bypass path is unaffected
- **Zero regression** for any object size ≥ 1 MB

#### Using `generate_buffer()` (automatic, no changes needed):

For object sizes < 1 MB without NUMA pinning, `generate_buffer()` **automatically uses the pool**. No code change required:

```python
import dgen_py

# This call is now 16× faster for 64 KB — no API change required
buf = dgen_py.generate_buffer(64 * 1024)
print(len(buf))  # 65536
print(type(buf)) # <class 'dgen_py.BytesView'> — zero-copy Python buffer

# Works exactly as before for large objects
big = dgen_py.generate_buffer(4 * 1024 * 1024)  # 4 MB — no change
```

#### Using `BufferPool` (explicit pool for tight loops):

`BufferPool` gives you **explicit control** over the pool for scenarios where you pre-generate many small objects in a loop — for example, image simulation or small-object PUT benchmarks:

```python
import dgen_py

# Create a pool with your desired data characteristics
pool = dgen_py.BufferPool(
    dedup_ratio=1.0,      # No deduplication
    compress_ratio=1.0    # Incompressible
)

# Generate 10,000 × 64 KB image-like objects
# Each next_slice() is a zero-copy Bytes window — no allocation cost
images = [pool.next_slice(64 * 1024) for _ in range(10_000)]
print(f"Generated {sum(len(img) for img in images) / 1e6:.1f} MB")  # 640.0 MB

# Slices behave like bytes — pass directly to your storage client
for img in images:
    # e.g. s3_client.put_object(Body=img, ...)
    pass
```

**Pool property accessors:**

```python
pool = dgen_py.BufferPool(dedup_ratio=2.0, compress_ratio=4.0)

print(pool.remaining)      # bytes left before next refill (0..1_048_576)
print(pool.dedup_ratio)    # int, current dedup factor
print(pool.compress_ratio) # int, current compress factor
```

**Changing data characteristics mid-stream:**

```python
pool = dgen_py.BufferPool()

# Generate incompressible objects
for _ in range(1000):
    obj = pool.next_slice(64 * 1024)

# Switch to compressible — forces one pool refill, then continues zero-copy
pool.reconfigure(dedup_ratio=1.0, compress_ratio=4.0)

for _ in range(1000):
    obj = pool.next_slice(64 * 1024)  # now compressible data
```

**When to use `BufferPool` vs `generate_buffer()`:**

| Scenario | Best choice |
|----------|-------------|
| Existing code, small objects < 1 MB | `generate_buffer()` — automatic, no code change |
| New code, tight loop, many small objects | `BufferPool` — explicit pool, identical performance |
| Large objects ≥ 1 MB | Either — both use the same large-object bypass path |
| NUMA-pinned workloads | `generate_buffer(..., numa_node=N)` — pool is bypassed per design |

### Version 0.2.0: Ultra-Fast Bulk Buffer Allocation 🎉

For scenarios where you need to **pre-generate all data in memory** before writing, use `create_bytearrays()` for **1,280x faster allocation** than Python list comprehension:

```python
import dgen_py
import time

# Pre-generate 24 GB in 32 MB chunks 
total_size = 24 * 1024**3  # 24 GB
chunk_size = 32 * 1024**2  # 32 MB chunks
num_chunks = total_size // chunk_size  # 768 chunks

# ✅ FAST: Rust-optimized allocation (7-11 ms for 24 GB!)
start = time.perf_counter()
chunks = dgen_py.create_bytearrays(count=num_chunks, size=chunk_size)
alloc_time = time.perf_counter() - start
print(f"Allocation: {alloc_time*1000:.1f} ms @ {(total_size/(1024**3))/alloc_time:.0f} GB/s")

# Fill buffers with high-performance generation
gen = dgen_py.Generator(size=total_size, numa_mode="auto", max_threads=None)

start = time.perf_counter()
for buf in chunks:
    gen.fill_chunk(buf)
gen_time = time.perf_counter() - start
print(f"Generation: {gen_time:.2f}s @ {(total_size/(1024**3))/gen_time:.1f} GB/s")

# Now write to storage...
# for buf in chunks:
#     f.write(buf)
```

**Performance (12-core system):**
```
Allocation: 10.9 ms @ 2204 GB/s  # 1,280x faster than Python!
Generation: 1.59s @ 15.1 GB/s
```

**Performance comparison:**
| Method | Allocation Time (24 GB) | Speedup |
|--------|------------------------|---------|
| Python `[bytearray(size) for _ in ...]` | 12-14 seconds | 1x (baseline) |
| `dgen_py.create_bytearrays()` | **7-11 ms** | **1,280x faster** |

**When to use:**
- ✅ Pre-generation pattern (DLIO benchmark, batch data loading)
- ✅ Need all data in RAM before writing
- ❌ Streaming - use `Generator.fill_chunk()` with reusable buffer instead (see below)

**Why it's fast:**
- Uses Python C API (`PyByteArray_Resize`) directly from Rust
- For 32 MB chunks, glibc automatically uses `mmap` (≥128 KB threshold)
- Zero-copy kernel page allocation, no heap fragmentation
- Bypasses Python interpreter overhead

### Version 0.1.7: Dynamic Seed Changes

Dynamically change the random seed to **reset the data stream** or create **alternating patterns** without recreating the Generator:

```python
import dgen_py

gen = dgen_py.Generator(size=100 * 1024**3, seed=1111)
buffer = bytearray(10 * 1024**2)

# Generate data with seed A
gen.set_seed(1111)
gen.fill_chunk(buffer)  # Pattern A

# Switch to seed B
gen.set_seed(2222)
gen.fill_chunk(buffer)  # Pattern B

# Back to seed A - resets the stream!
gen.set_seed(1111)
gen.fill_chunk(buffer)  # SAME as first chunk (pattern A)
```

**Use cases:**
- RAID stripe testing with alternating patterns per drive
- Multi-phase AI/ML workloads (different patterns for metadata/payload/footer)
- Complex reproducible benchmark scenarios
- Low-overhead stream reset (no Generator recreation)

### Version 0.1.6: Reproducible Data Generation

Generate **identical data across runs** for reproducible benchmarking and testing:

```python
import dgen_py

# Reproducible mode - same seed produces identical data
gen1 = dgen_py.Generator(size=10 * 1024**3, seed=12345)
gen2 = dgen_py.Generator(size=10 * 1024**3, seed=12345)
# ⇒ gen1 and gen2 produce IDENTICAL data streams

# Non-deterministic mode (default) - different data each run  
gen3 = dgen_py.Generator(size=10 * 1024**3)  # seed=None (default)
```

**Use cases:**
- 🔬 Reproducible benchmarking: Compare storage systems with identical workloads
- ✅ Consistent testing: Same test data across CI/CD pipeline runs
- 🐛 Debugging: Regenerate exact data streams for issue investigation
- 📊 Compliance: Verifiable data generation for audits

### Streaming API (Basic Usage)

For **unlimited data generation with constant memory usage**, use the streaming API:

```python
import dgen_py
import time

# Generate 100 GB with streaming (only 32 MB in memory at a time)
gen = dgen_py.Generator(
    size=100 * 1024**3,      # 100 GB total
    dedup_ratio=1.0,         # No deduplication 
    compress_ratio=1.0,      # Incompressible data
    numa_mode="auto",        # Auto-detect NUMA topology
    max_threads=None         # Use all available cores
)

# Create single reusable buffer
buffer = bytearray(gen.chunk_size)

# Stream data in chunks (zero-copy, parallel generation)
start = time.perf_counter()
while not gen.is_complete():
    nbytes = gen.fill_chunk(buffer)
    if nbytes == 0:
        break
    # Write to file/network: buffer[:nbytes]

duration = time.perf_counter() - start
print(f"Throughput: {(100 / duration):.2f} GB/s")
```

**Example output (8-core system):**
```
Throughput: 86.41 GB/s
```

**When to use:**
- ✅ Generating very large datasets (> available RAM)
- ✅ Consistent low memory footprint (32 MB)
- ✅ Network streaming, continuous data generation

### System Information

```python
import dgen_py

info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")
```

## Advanced Usage

### Multi-Process NUMA (For Multi-NUMA Systems)

For maximum throughput on multi-socket systems, use **one Python process per NUMA node** with process affinity pinning.

**See [python/examples/benchmark_numa_multiprocess_v2.py](python/examples/benchmark_numa_multiprocess_v2.py) for complete implementation.**

Key architecture:
- One Python process per NUMA node
- Process pinning via `os.sched_setaffinity()` to local cores
- Local memory allocation on each NUMA node
- Synchronized start with multiprocessing.Barrier

**Results**:
- C4-96 (48 cores, 2 NUMA nodes): 248.53 GB/s aggregate
- C4-32 (16 cores, 1 NUMA node): 162.78 GB/s with 112% scaling efficiency

### Chunk Size Optimization

**Default chunk size is automatically optimized** for your system. You can override if needed:

```python
gen = dgen_py.Generator(
    size=100 * 1024**3,
    chunk_size=64 * 1024**2  # Override to 64 MB
)
```

**Newer CPUs** (Emerald Rapid, Sapphire Rapids) with larger L3 cache benefit from 64 MB chunks.

### Deduplication and Compression Ratios

**Performance vs Test Accuracy Tradeoff**:

```python
# FAST: Incompressible data (1.0x baseline)
gen = dgen_py.Generator(
    size=100 * 1024**3,
    dedup_ratio=1.0,      # No dedup (no performance impact)
    compress_ratio=1.0    # Incompressible data
)

# FASTER: More compressible (1.3-1.5x speedup)
gen = dgen_py.Generator(
    size=100 * 1024**3,
    dedup_ratio=1.0,      # No dedup (no performance impact)
    compress_ratio=2.0    # 2:1 compressible data
)
```

**Important**: Higher `compress_ratio` values improve generation performance (1.3-1.5x faster) BUT make the data more compressible, which may not represent your actual workload:

- **compress_ratio=1.0**: Incompressible data (realistic for encrypted files, compressed archives)
- **compress_ratio=2.0**: 2:1 compressible data (realistic for text, logs, uncompressed images)
- **compress_ratio=3.0+**: Highly compressible data (may not be realistic)

**Choose based on YOUR test requirements**, not performance numbers. If testing storage with compression enabled, use compress_ratio=1.0 to avoid inflating storage efficiency metrics.

**Note**: `dedup_ratio` has zero performance impact (< 1% variance)

### NUMA Modes

```python
# Auto-detect topology (recommended)
gen = dgen_py.Generator(..., numa_mode="auto")

# Force UMA (single-socket)
gen = dgen_py.Generator(..., numa_mode="uma")

# Manual NUMA node binding (multi-process only)
gen = dgen_py.Generator(..., numa_node=0)  # Bind to node 0
```

## Architecture

### Zero-Copy Implementation

**Python buffer protocol** with direct memory access:
- No data copying between Rust and Python
- GIL released during generation (true parallelism)
- Memoryview creation < 0.001ms (verified zero-copy)

### Parallel Generation

- **4 MiB internal blocks** distributed across all cores
- Thread pool created once, reused for all operations
- Xoshiro256++ RNG (5-10x faster than ChaCha20)
- Optimal for L3 cache performance

### NUMA Optimization

- Multi-process architecture (one process per NUMA node)
- Local memory allocation on each node
- Local core affinity (no cross-node traffic)
- Automatic topology detection via hwloc

## Use Cases

- **Storage benchmarking**: Generate realistic test data at 40-188 GB/s
- **Network testing**: High-throughput data sources
- **AI/ML profiling**: Simulate data loading pipelines
- **Compression testing**: Validate compressor behavior with controlled ratios
- **Deduplication testing**: Test dedup systems with known ratios

## License

Dual-licensed under MIT OR Apache-2.0

## Credits

- Built with [PyO3](https://pyo3.rs/) and [Maturin](https://www.maturin.rs/)
- Uses [hwlocality](https://crates.io/crates/hwlocality) for NUMA topology detection
- Xoshiro256++ RNG from [rand](https://crates.io/crates/rand) crate