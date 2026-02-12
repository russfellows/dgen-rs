# dgen-py

**The worlds fastest Python random data generation - with NUMA optimization and zero-copy interface**

[![Version](https://img.shields.io/badge/version-0.2.0-blue)](https://pypi.org/project/dgen-py/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![PyPI](https://img.shields.io/pypi/v/dgen-py)](https://pypi.org/project/dgen-py/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org)
[![Tests](https://img.shields.io/badge/tests-6%20passing-success)](https://github.com/russfellows/dgen-rs)

## Features

- 🚀 **Blazing Fast**: 10 GB/s per core, up to 300 GB/s verified
- ⚡ **Ultra-Fast Allocation**: `create_bytearrays()` for 1,280x faster pre-allocation than Python (NEW in v0.2.0)
- 🎯 **Controllable Characteristics**: Configurable deduplication and compression ratios
- 🔄 **Reproducible Data**: Seed parameter for identical data generation (v0.1.6) with dynamic reseeding (v0.1.7)
- 🔬 **Multi-Process NUMA**: One Python process per NUMA node for maximum throughput
- 🐍 **True Zero-Copy**: Python buffer protocol with direct memory access (no data copying)
- 📦 **Streaming API**: Generate terabytes of data with constant 32 MB memory usage
- 🧵 **Thread Pool Reuse**: Created once, reused across all operations
- 🛠️ **Built with Rust**: Memory-safe, production-quality implementation

## Performance

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

### System Requirements

**For NUMA support (Linux only):**
```bash
# Ubuntu/Debian
sudo apt-get install libudev-dev libhwloc-dev

# RHEL/CentOS/Fedora
sudo yum install systemd-devel hwloc-devel
```

**Note**: NUMA support is optional. Without these libraries, the package works perfectly on single-NUMA systems (workstations, cloud VMs).

## Quick Start

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