# dgen-rs / dgen-py

**High-performance random data generation with controllable deduplication, compression, and NUMA optimization**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.90+-orange.svg)](https://www.rust-lang.org)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org)
[![Tests](https://img.shields.io/badge/tests-14%20passing-brightgreen.svg)](#testing)

## Features

- 🚀 **Blazing Fast**: 5-15 GB/s per core using Xoshiro256++ RNG
- 🎯 **Controllable Characteristics**: 
  - Deduplication ratios (1:1 to N:1)
  - Compression ratios (1:1 to N:1)
- 🔬 **NUMA-Aware**: Automatic topology detection and optimization on multi-socket systems
- 🐍 **Zero-Copy Python API**: Direct buffer writes, no unnecessary copies
- 📦 **Both Simple and Streaming**: Single-call or incremental generation
- 🛠️ **Built with Rust**: Memory-safe, production-quality code

## Quick Start

### Python Installation

```bash
# Install from PyPI (when published)
pip install dgen-py

# Or build from source
cd dgen-rs
maturin develop --release
```

### Python Usage

**Simple API** (generate all at once):

```python
import dgen_py

# Generate 100 MiB incompressible data
data = dgen_py.generate_data(100 * 1024 * 1024)
print(f"Generated {len(data)} bytes")

# Generate with 2:1 dedup and 3:1 compression
data = dgen_py.generate_data(
    size=100 * 1024 * 1024,
    dedup_ratio=2.0,
    compress_ratio=3.0
)
```

**Zero-Copy API** (write into existing buffer):

```python
import dgen_py
import numpy as np

# Pre-allocate buffer
buf = bytearray(1024 * 1024)

# Generate directly into buffer (zero-copy!)
nbytes = dgen_py.fill_buffer(buf, compress_ratio=2.0)
print(f"Wrote {nbytes} bytes")

# Works with NumPy arrays
arr = np.zeros(100 * 1024 * 1024, dtype=np.uint8)
dgen_py.fill_buffer(arr, dedup_ratio=2.0, compress_ratio=3.0)
```

**Streaming API** (incremental generation):

```python
import dgen_py

# Create generator for 1 GiB
gen = dgen_py.Generator(
    size=1024 * 1024 * 1024,
    dedup_ratio=2.0,
    compress_ratio=3.0,
    numa_aware=True  # Auto-detected by default
)

# Generate in chunks
chunk_size = 8192
buf = bytearray(chunk_size)
total = 0

while not gen.is_complete():
    nbytes = gen.fill_chunk(buf)
    if nbytes == 0:
        break
    
    total += nbytes
    # Process chunk (write to file, network, etc.)
    # ... 

print(f"Generated {total} bytes")
```

**NUMA Information**:

```python
import dgen_py

info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")
```

### Rust Usage

```rust
use dgen_rs::{generate_data_simple, GeneratorConfig, DataGenerator};

// Simple API
let data = generate_data_simple(100 * 1024 * 1024, 1, 1);

// Full configuration
let config = GeneratorConfig {
    size: 100 * 1024 * 1024,
    dedup_factor: 2,
    compress_factor: 3,
    numa_aware: true,
};
let data = dgen_rs::generate_data(config);

// Streaming
let mut gen = DataGenerator::new(config);
let mut chunk = vec![0u8; 8192];
while !gen.is_complete() {
    let written = gen.fill_chunk(&mut chunk);
    if written == 0 {
        break;
    }
    // Process chunk...
}
```

## How It Works

### Deduplication

Deduplication ratio `N` means:
- Generate `total_blocks / N` unique blocks
- Reuse blocks in round-robin fashion
- Example: 100 blocks, dedup=2 → 50 unique blocks, repeated 2x each

### Compression

Compression ratio `N` means:
- Fill block with high-entropy Xoshiro256++ keystream
- Add local back-references to achieve N:1 compressibility
- Example: compress=3 → zstd will compress to ~33% of original size

**compress=1**: Truly incompressible (zstd ratio ~1.00-1.02)  
**compress>1**: Target ratio via local back-refs, evenly distributed

### NUMA Optimization

On multi-socket systems (NUMA nodes > 1):
- Detects topology via `/sys/devices/system/node` (Linux)
- Can pin rayon threads to specific NUMA nodes (optional)
- Ensures memory locality for maximum bandwidth

## Performance

Typical throughput on modern CPUs:

- **Incompressible** (compress=1): 5-15 GB/s per core
- **Compressible** (compress=3): 1-4 GB/s per core
- **Multi-core**: Near-linear scaling with rayon

Benchmark on AMD EPYC 7742 (64 cores):
```
Incompressible:  ~500 GB/s (all cores)
Compress 3:1:    ~150 GB/s (all cores)
```

## Algorithm Details

Based on s3dlio's `data_gen_alt.rs`:

1. **Block-level generation**: 4 MiB blocks processed in parallel
2. **Xoshiro256++**: 5-10x faster than ChaCha20, cryptographically strong
3. **Integer error accumulation**: Even compression distribution
4. **No cross-block compression**: Realistic compressor behavior
5. **Per-call entropy**: Unique data across distributed nodes

## Use Cases

- **Storage benchmarking**: Generate realistic test data
- **Network testing**: High-throughput data sources
- **AI/ML profiling**: Simulate data loading pipelines
- **Compression testing**: Validate compressor behavior
- **Deduplication testing**: Test dedup ratios

## Building from Source

```bash
# Clone repository
git clone https://github.com/russfellows/dgen-rs.git
cd dgen-rs

# Build Rust library
cargo build --release

# Build Python wheel
maturin build --release

# Install locally
maturin develop --release

# Run tests
cargo test
python -m pytest python/tests/
```

## Requirements

- **Rust**: 1.90+ (edition 2021)
- **Python**: 3.10+ (for Python bindings)
- **Platform**: Linux (NUMA detection required)

## License

Dual-licensed under MIT OR Apache-2.0

## Credits

- Data generation algorithm ported from [s3dlio](https://github.com/russfellows/s3dlio)
- Built with [PyO3](https://pyo3.rs/) and [Maturin](https://www.maturin.rs/)

## See Also

- **s3dlio**: High-performance multi-protocol storage I/O
- **sai3-bench**: Multi-protocol I/O benchmarking suite
- **kv-cache-bench**: LLM KV cache storage benchmarking

