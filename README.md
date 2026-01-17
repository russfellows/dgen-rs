# dgen-rs / dgen-py

**High-performance random data generation with controllable deduplication, compression, and NUMA optimization**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.90+-orange.svg)](https://www.rust-lang.org)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org)
[![Version](https://img.shields.io/badge/version-0.1.3-blue.svg)](#)

## Features

- 🚀 **Blazing Fast**: 40-50 GB/s on 12 cores (3.5-4 GB/s per core) - scales linearly to 1,500+ GB/s on 384 cores
- 🎯 **Controllable Characteristics**: 
  - Deduplication ratios (1:1 to N:1)
  - Compression ratios (1:1 to N:1)
- 🔬 **NUMA-Aware**: Automatic topology detection and optimization on multi-socket systems
- 🐍 **True Zero-Copy Python API**: Direct buffer writes with GIL release for maximum performance
- 📦 **Both One-Shot and Streaming**: Single-call or incremental generation with parallel execution
- 🧵 **Thread Pool Reuse**: Created once, reused for all operations (eliminates overhead)
- 🛠️ **Built with Rust**: Memory-safe, production-quality code

## Performance

**Development System (12 cores):**
- Python: 43.25 GB/s (3.60 GB/s per core)
- Native Rust: 47.18 GB/s (3.93 GB/s per core)

**HPC System (384 cores, projected):**
- Expected throughput: 1,384-1,500 GB/s
- Perfect for high-speed storage testing (easily exceeds 80 GB/s targets)

## Quick Start

### Python Installation

```bash
# Install from PyPI (when published)
pip install dgen-py

# Or build from source
cd dgen-rs
./build_pyo3.sh
pip install ./target/wheels/*.whl
```

### Python Usage

**Simple API** (generate all at once):

```python
import dgen_py

# Generate 100 MiB incompressible data
data = dgen_py.generate_buffer(100 * 1024 * 1024)
print(f"Generated {len(data)} bytes")

# Generate with 2:1 dedup and 3:1 compression
data = dgen_py.generate_buffer(
    size=100 * 1024 * 1024,
    dedup_ratio=2.0,
    compress_ratio=3.0,
    numa_mode="auto",
    max_threads=None  # Use all cores
)
```

**Zero-Copy API** (write into existing buffer):

```python
import dgen_py

# Pre-allocate buffer
buf = bytearray(64 * 1024 * 1024)  # 64 MB

# Generate directly into buffer (TRUE zero-copy!)
nbytes = dgen_py.generate_into_buffer(
    buf, 
    dedup_ratio=1.0,
    compress_ratio=1.0,
    numa_mode="auto",
    max_threads=None
)
print(f"Wrote {nbytes} bytes")
```

**Streaming API** (incremental generation with parallel execution):

```python
import dgen_py

# Create generator for 1 TB
gen = dgen_py.Generator(
    size=1024**4,  # 1 TB
    dedup_ratio=1.0,
    compress_ratio=1.0,
    numa_mode="auto",  # Auto-detect NUMA topology
    max_threads=None   # Use all cores
)

# Use large chunks for parallel generation (64+ MB recommended)
chunk_size = 64 * 1024 * 1024  # 64 MB = 16 parallel blocks
buf = bytearray(chunk_size)

while not gen.is_complete():
    nbytes = gen.fill_chunk(buf)  # Zero-copy parallel generation
    if nbytes == 0:
        break
    
    # Write to storage (buf[:nbytes])
    # file.write(buf[:nbytes])

# Expected performance: 40-50 GB/s on 12 cores, 1,500+ GB/s on 384 cores
```

**Key Performance Tips:**
- Use **64-256 MB chunks** for streaming (enables parallel generation)
- Chunks < 8 MB fall back to sequential (slower)
- `numa_mode="auto"` optimizes for multi-socket systems
- Thread pool is reused across all `fill_chunk()` calls (zero overhead)

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

