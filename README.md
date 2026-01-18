# dgen-py

**High-performance random data generation with NUMA optimization and zero-copy Python interface**

[![Version](https://img.shields.io/badge/version-0.1.4-blue)](https://pypi.org/project/dgen-py/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![PyPI](https://img.shields.io/pypi/v/dgen-py)](https://pypi.org/project/dgen-py/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org)

## Features

- 🚀 **Blazing Fast**: 40+ GB/s on 12 cores, 126 GB/s on 96 cores, 188 GB/s on 368 cores
- 🎯 **Controllable Characteristics**: Configurable deduplication and compression ratios
- 🔬 **Multi-Process NUMA**: One Python process per NUMA node for maximum throughput
- 🐍 **True Zero-Copy**: Python buffer protocol with direct memory access (no data copying)
- 📦 **Streaming API**: Generate terabytes of data with constant memory usage
- 🧵 **Thread Pool Reuse**: Created once, reused across all operations
- 🛠️ **Built with Rust**: Memory-safe, production-quality implementation

## Performance

### Real-World Benchmarks (v0.1.3)

**Multi-NUMA Systems** (one Python process per NUMA node):

| System | Cores | NUMA Nodes | Throughput | Per-Core | Efficiency |
|--------|-------|------------|------------|----------|------------|
| **GCP C4-16** | 16 | 1 (UMA) | 39.87 GB/s | 2.49 GB/s | 100% (baseline) |
| **GCP C4-96** | 96 | 4 | 126.96 GB/s | 1.32 GB/s | 53% |
| **Azure HBv5** | 368 | 16 | 188.24 GB/s | 0.51 GB/s | 20% |

**Single-NUMA Systems** (one Python process):

| System | Cores | Throughput | Per-Core | Notes |
|--------|-------|------------|----------|-------|
| **Workstation** | 12 | 41.23 GB/s | 3.44 GB/s | Development system, UMA |

**Key Findings:**
- Sub-linear scaling is **expected** for memory-intensive workloads (memory bandwidth bottleneck)
- All systems **far exceed 80 GB/s** storage testing requirements
- Maximum throughput: 188 GB/s on 368-core HBv5 system
- Excellent single-node performance: 40+ GB/s on commodity hardware

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

### Basic Usage (Fastest - No Dedup/Compression)

```python
import dgen_py

# Generate 100 GB of random data (incompressible, no dedup)
gen = dgen_py.Generator(
    size=100 * 1024**3,      # 100 GB
    dedup_ratio=1.0,         # No deduplication (fastest)
    compress_ratio=1.0,      # Incompressible (fastest)
    numa_mode="auto",        # Auto-detect NUMA topology
    max_threads=None         # Use all available cores
)

# Create buffer (uses optimal 32 MB chunk size)
buffer = bytearray(gen.chunk_size)

# Stream data in chunks (zero-copy, parallel generation)
while not gen.is_complete():
    nbytes = gen.fill_chunk(buffer)
    if nbytes == 0:
        break
    # Write to file/network: buffer[:nbytes]
```

### Performance Example (Actual Results)

```python
import dgen_py
import time

# 100 GB incompressible test
TEST_SIZE = 100 * 1024**3

gen = dgen_py.Generator(
    size=TEST_SIZE,
    dedup_ratio=1.0,         # No deduplication
    compress_ratio=1.0,      # Incompressible
    numa_mode="auto",
    max_threads=None
)

buffer = bytearray(gen.chunk_size)
start = time.perf_counter()

while not gen.is_complete():
    nbytes = gen.fill_chunk(buffer)
    if nbytes == 0:
        break

duration = time.perf_counter() - start
throughput = (TEST_SIZE / 1024**3) / duration

print(f"Duration: {duration:.2f} seconds")
print(f"Throughput: {throughput:.2f} GB/s")
```

**Complete benchmark output (12-core workstation):**

```
NUMA nodes: 1
Physical cores: 12
Deployment: UMA (single NUMA node - cloud VM or workstation)

Starting Benchmark: 3 runs of 100 GB each
Using ZERO-COPY PARALLEL STREAMING

============================================================
TEST 1: DEFAULT CHUNK SIZE (should use optimal 32 MB)
============================================================
Using chunk size: 32 MB
------------------------------------------------------------
Run 01: 3.0401 seconds | 32.89 GB/s
Run 02: 2.1536 seconds | 46.43 GB/s
Run 03: 2.0826 seconds | 48.02 GB/s
------------------------------------------------------------
AVERAGE DURATION:   2.4254 seconds
AVERAGE THROUGHPUT: 41.23 GB/s
PER-CORE THROUGHPUT: 3.44 GB/s

============================================================
TEST 2: OVERRIDE CHUNK SIZE TO 64 MB
============================================================
Using chunk size: 64 MB
------------------------------------------------------------
Run 01: 2.2696 seconds | 44.06 GB/s
Run 02: 2.2647 seconds | 44.16 GB/s
Run 03: 2.2709 seconds | 44.04 GB/s
------------------------------------------------------------
AVERAGE DURATION:   2.2684 seconds
AVERAGE THROUGHPUT: 44.08 GB/s
PER-CORE THROUGHPUT: 3.67 GB/s

============================================================
COMPARISON
============================================================
32 MB (default): 41.23 GB/s
64 MB (override): 44.08 GB/s
64 MB is 6.5% faster than 32 MB

OPTIMIZATION NOTES:
  - Thread pool created ONCE and reused
  - ZERO-COPY: Generates directly into output buffer
  - Internal parallelization: 4 MiB blocks (optimal for L3 cache)
  - Parallel generation distributes blocks across all available cores
```

### System Information

```python
import dgen_py

info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")

# Example output (12-core workstation):
# NUMA nodes: 1
# Physical cores: 12
# Deployment: UMA (single NUMA node - cloud VM or workstation)
```

## Advanced Usage

### Multi-Process NUMA (For Multi-NUMA Systems)
}
```

### Multi-Process NUMA (For Multi-NUMA Systems)

For maximum throughput on multi-socket systems, use **one Python process per NUMA node**:

```python
from multiprocessing import Process, Queue, Barrier
import dgen_py

def worker_process(numa_node: int, barrier: Barrier, result_queue: Queue):
    """One process per NUMA node for maximum performance"""
    gen = dgen_py.Generator(
        size=100 * 1024**3,      # 100 GB per process
        dedup_ratio=1.0,         # No deduplication
        compress_ratio=1.0,      # Incompressible
        numa_node=numa_node,     # Bind to specific NUMA node
        max_threads=None
    )
    
    buffer = bytearray(gen.chunk_size)
    barrier.wait()  # Synchronized start
    
    start = time.perf_counter()
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
        # Write buffer[:nbytes] to storage
    
    duration = time.perf_counter() - start
    result_queue.put({'numa_node': numa_node, 'duration': duration})

# Detect NUMA topology
num_numa_nodes = dgen_py.detect_numa_nodes()

# Spawn one process per NUMA node
barrier = Barrier(num_numa_nodes)
result_queue = Queue()

processes = [
    Process(target=worker_process, args=(i, barrier, result_queue))
    for i in range(num_numa_nodes)
]

for p in processes:
    p.start()

for p in processes:
    p.join()

# Collect results
# On C4-96 (4 NUMA nodes): 126.96 GB/s aggregate
# On HBv5 (16 NUMA nodes): 188.24 GB/s aggregate
```

## Performance Notes

### Chunk Size Optimization

**32 MB chunks are optimal** (default), but you can override:

```python
gen = dgen_py.Generator(
    size=100 * 1024**3,
    dedup_ratio=1.0,
    compress_ratio=1.0,
    chunk_size=64 * 1024**2  # Override to 64 MB
)
```

**Benchmark results (12-core workstation, 100 GB test):**
- **32 MB chunks**: 41.23 GB/s (3.44 GB/s per core)
- **64 MB chunks**: 44.08 GB/s (3.67 GB/s per core)
- **Difference**: 64 MB is 6.5% faster on this system

### Deduplication and Compression

For **maximum performance**, use `dedup_ratio=1.0` and `compress_ratio=1.0`:

```python
# FASTEST: No deduplication, incompressible
gen = dgen_py.Generator(
    size=100 * 1024**3,
    dedup_ratio=1.0,      # No dedup (fastest)
    compress_ratio=1.0    # Incompressible (fastest)
)
```

Higher ratios reduce throughput:

```python
# SLOWER: With dedup and compression
gen = dgen_py.Generator(
    size=100 * 1024**3,
    dedup_ratio=2.0,      # 2:1 deduplication
    compress_ratio=3.0    # 3:1 compression
)
# Throughput will be lower due to processing overhead
```

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