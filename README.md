# dgen-py

**High-performance random data generation with NUMA optimization and zero-copy Python interface**

[![Version](https://img.shields.io/badge/version-0.1.6-blue)](https://pypi.org/project/dgen-py/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![PyPI](https://img.shields.io/pypi/v/dgen-py)](https://pypi.org/project/dgen-py/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org)
[![Tests](https://img.shields.io/badge/tests-5%20passing-success)](https://github.com/russfellows/dgen-rs)

## Features

- 🚀 **Blazing Fast**: 58+ GB/s streaming throughput, matches Numba JIT performance
- 🎯 **Controllable Characteristics**: Configurable deduplication and compression ratios
- 🔄 **Reproducible Data**: Optional seed parameter for identical data generation across runs
- 🔬 **Multi-Process NUMA**: One Python process per NUMA node for maximum throughput
- 🐍 **True Zero-Copy**: Python buffer protocol with direct memory access (no data copying)
- 📦 **Streaming API**: Generate terabytes of data with constant 32 MB memory usage
- 🧵 **Thread Pool Reuse**: Created once, reused across all operations
- 🛠️ **Built with Rust**: Memory-safe, production-quality implementation

## Performance

### Version 0.1.5 Highlights 🎉

**NEW: Significant Performance Improvements** over v0.1.3:
- **UMA systems**: ~50% improvement in per-core throughput (10.80 GB/s vs ~7 GB/s)
- **NUMA systems**: Major improvements from bug fixes in multi-process architecture
- **8-core system**: **86.41 GB/s** aggregate throughput (C4-16)
- **Maximum aggregate**: **324.72 GB/s** on 48-core dual-NUMA system (C4-96 with compress=2.0)

### Streaming Benchmark (v0.1.5) - 100 GB Test

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

### Multi-NUMA Benchmarks (v0.1.5) - GCP Emerald Rapid

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

### Basic Usage

```python
import dgen_py
import time

# Generate 100 GB of random data with configurable characteristics
gen = dgen_py.Generator(
    size=100 * 1024**3,      # 100 GB
    dedup_ratio=1.0,         # No deduplication 
    compress_ratio=1.0,      # Incompressible 
    numa_mode="auto",        # Auto-detect NUMA topology
    max_threads=None         # Use all available cores
)

# Create buffer (uses optimal chunk size automatically)
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

### Reproducible Data Generation (NEW in v0.1.6)

```python
import dgen_py

# Generate reproducible data with a fixed seed
gen1 = dgen_py.Generator(
    size=10 * 1024**3,  # 10 GB
    seed=12345          # Optional: enables reproducibility
)

# Same seed produces identical data
gen2 = dgen_py.Generator(
    size=10 * 1024**3,
    seed=12345          # Same seed = identical data
)

# Without seed (default), data is non-deterministic
gen3 = dgen_py.Generator(
    size=10 * 1024**3   # seed=None (default)
)
```

**Use cases for reproducible mode:**
- Reproducible benchmarking and testing
- Consistent test data across CI/CD runs
- Debugging with identical data streams
- Verifiable data generation for compliance

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

## Performance Notes

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