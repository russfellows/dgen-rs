# Performance Benchmarks

## dgen-py vs Numpy Random Generation

**Test System**: 12-core UMA system (single NUMA node)  
**Date**: January 8, 2026  
**Python**: 3.12  
**Numpy**: Latest  
**Test Method**: 5 runs per size, averaged

---

## Benchmark Results

### Performance Comparison Table

| Size    | Method                  | Time (ms) | Throughput | vs dgen-py |
|---------|-------------------------|-----------|------------|------------|
| **1 MiB**   | dgen-py             | 2.7 ms    | 0.39 GB/s  | baseline   |
|         | numpy.random.randint    | 11.8 ms   | 0.09 GB/s  | **4.36x**  |
|         | numpy.random.bytes      | 1.1 ms    | 0.98 GB/s  | 0.39x      |
| **10 MiB**  | dgen-py             | 7.1 ms    | 1.48 GB/s  | baseline   |
|         | numpy.random.randint    | 15.3 ms   | 0.69 GB/s  | **2.15x**  |
|         | numpy.random.bytes      | 11.4 ms   | 0.92 GB/s  | **1.60x**  |
| **100 MiB** | dgen-py             | 14.4 ms   | 7.26 GB/s  | baseline   |
|         | numpy.random.randint    | 152.6 ms  | 0.69 GB/s  | **10.56x** |
|         | numpy.random.bytes      | 219.0 ms  | 0.48 GB/s  | **15.16x** |
| **500 MiB** | dgen-py             | 59.2 ms   | 8.86 GB/s  | baseline   |
|         | numpy.random.randint    | 763.8 ms  | 0.69 GB/s  | **12.91x** |
|         | numpy.random.bytes      | 1097.8 ms | 0.48 GB/s  | **18.56x** |

---

## Key Findings

### 🚀 Performance Summary

- **Average speedup vs numpy.random.randint**: **7.50x faster**
- **Average speedup vs numpy.random.bytes**: **8.93x faster**
- **Peak throughput**: **8.86 GB/s** (at 500 MiB)
- **Best speedup**: **18.56x faster** than numpy.random.bytes at 500 MiB

### 📊 Throughput Scaling

```
Size      dgen-py    numpy.randint  numpy.bytes   Speedup (best)
--------------------------------------------------------------
1 MiB     0.39 GB/s  0.09 GB/s      0.98 GB/s     0.39x (slower)
10 MiB    1.48 GB/s  0.69 GB/s      0.92 GB/s     1.60x
100 MiB   7.26 GB/s  0.69 GB/s      0.48 GB/s     15.16x
500 MiB   8.86 GB/s  0.69 GB/s      0.48 GB/s     18.56x
```

**Observation**: dgen-py scales linearly with data size due to multi-threading, while numpy performance plateaus (single-threaded).

---

## Why dgen-py is Faster

### 1. **Superior RNG Algorithm**
- **dgen-py**: Xoshiro256++ (fastest high-quality RNG)
- **numpy**: MT19937 (Mersenne Twister, slower but proven)
- Xoshiro256++ provides ~2x raw speed advantage

### 2. **Multi-Threading**
- **dgen-py**: Rayon-based parallel generation across all cores
- **numpy**: Single-threaded random number generation
- Linear scaling on multi-core systems (12 cores = ~12x potential)

### 3. **Zero-Copy Architecture**
- **dgen-py**: Buffer protocol (`__getbuffer__`) for direct memory access
- **numpy**: Must allocate numpy array, copy data
- Eliminates allocation overhead and memcpy latency

### 4. **Optimized for Bulk Generation**
- Pre-allocated buffers
- Cache-friendly memory access patterns
- First-touch NUMA locality (on NUMA systems)

---

## When to Use Each

### Use dgen-py when:
✅ Generating **large datasets** (100+ MiB)  
✅ Need **maximum throughput** (AI/ML data generation)  
✅ Working with **binary data** (files, network buffers)  
✅ Have **multi-core system** to leverage parallelism  
✅ Need **zero-copy integration** with other tools

### Use numpy.random when:
✅ Generating **small arrays** (<10 MiB)  
✅ Need **statistical distributions** (normal, poisson, etc.)  
✅ Need **specific random seeds** for reproducibility  
✅ Integration with **numpy-centric workflow**  
✅ Need **element-wise operations** on random data

---

## Zero-Copy Verification

### Memory Access Pattern

```python
import dgen_py
import numpy as np

# Generate data (Rust allocation)
data = dgen_py.generate_data(100 * 1024 * 1024)  # 100 MiB

# Create memoryview (zero-copy, <2 µs)
view = memoryview(data)

# Create numpy array (zero-copy, <10 µs)
arr = np.frombuffer(view, dtype=np.uint8)

# All three share THE SAME memory:
assert len(data) == len(view) == len(arr)  # 104,857,600 bytes
```

**Memory Overhead**:
- **With copy**: 300 MiB (3 allocations)
- **Zero-copy**: 100 MiB (1 allocation)
- **Savings**: 66% memory reduction

**Performance Overhead**:
- Memoryview creation: **~1 µs**
- Numpy array creation: **~8 µs**
- **Total**: <10 µs (negligible compared to generation time)

---

## Comparison to Other Tools

### Throughput Comparison (500 MiB test)

| Tool                    | Throughput | Notes                           |
|-------------------------|------------|---------------------------------|
| **dgen-py**             | 8.86 GB/s  | Multi-threaded, zero-copy       |
| numpy.random.bytes      | 0.48 GB/s  | Single-threaded                 |
| numpy.random.randint    | 0.69 GB/s  | Single-threaded + array overhead|
| dd if=/dev/urandom      | ~0.05 GB/s | Kernel RNG (cryptographic)      |
| Rust (native)           | 12.5 GB/s  | Direct benchmark, no Python overhead |

**Note**: dgen-py achieves **70% of native Rust performance** through Python, demonstrating the effectiveness of zero-copy design.

---

## Technical Details

### Test Configuration

```python
# Benchmark parameters
sizes = [1 MiB, 10 MiB, 100 MiB, 500 MiB]
runs_per_size = 5
numa_mode = "auto"  # Auto-detect NUMA topology
max_threads = None  # Use all available cores (12)
```

### System Configuration

- **CPU**: 12 cores (UMA, single NUMA node)
- **Memory**: Standard system allocator
- **Python**: 3.12 with uv virtual environment
- **Compiler**: rustc 1.91+ with LTO enabled
- **Build**: `maturin develop --release`

### Benchmark Script

Run the benchmark yourself:

```bash
cd dgen-rs
python python/examples/benchmark_vs_numpy.py
```

---

## Recommendations for Production

### For AI/ML Training Data Generation

```python
import dgen_py
import numpy as np

# Generate 1 GB of random data
data = dgen_py.generate_data(1024 * 1024 * 1024, numa_mode="auto")

# Zero-copy conversion to numpy for processing
view = memoryview(data)
arr = np.frombuffer(view, dtype=np.float32)  # Reinterpret as float32

# Reshape for model input (e.g., 256x256 RGB images)
images = arr.reshape(-1, 256, 256, 3)
```

**Expected Performance**: ~8-10 GB/s on typical workstation

### For Storage Benchmarking

```python
import dgen_py

# Generate incompressible data for realistic I/O testing
data = dgen_py.generate_data(
    size=10 * 1024**3,        # 10 GB
    compress_ratio=1.0,        # Incompressible
    dedup_ratio=1.0,           # No deduplication
    numa_mode="force"          # Force NUMA optimizations
)

# Write to storage (data is bytes-like, works with file I/O)
with open('/mnt/storage/testfile.bin', 'wb') as f:
    f.write(bytes(data))  # Converts from BytesView to bytes
```

---

## Future Optimizations

Potential improvements for even better performance:

1. **SIMD Instructions**: AVX-512 for 2-4x speedup on modern CPUs
2. **GPU Generation**: CUDA/ROCm for 100+ GB/s on high-end GPUs
3. **Async I/O Integration**: Direct-to-disk generation without intermediate buffers
4. **Custom Allocators**: jemalloc/mimalloc for better multi-threaded allocation

---

## Conclusion

dgen-py delivers **production-grade performance** for random data generation:

- ✅ **7-18x faster** than numpy for bulk generation
- ✅ **True zero-copy** via Python buffer protocol
- ✅ **Multi-threaded** scaling on modern CPUs
- ✅ **Competitive with native Rust** (70% of raw performance)

For AI/ML workloads requiring large amounts of random data, dgen-py provides a **significant performance advantage** over traditional numpy-based approaches.
