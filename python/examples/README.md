# Python Examples

Performance benchmark scripts to find optimal dgen-py settings for your system.

## Quick Start

```bash
# Create virtual environment with uv
uv venv --python 3.12
source .venv/bin/activate  # or: .venv\Scripts\activate on Windows

# Build and install dgen-py
maturin develop --release

# Run quick 30-second test
python python/examples/quick_perf_test.py
```

## Scripts

### 1. quick_perf_test.py - Fast Optimization (30 seconds)

Quick test to find the best settings for your system.

```bash
python python/examples/quick_perf_test.py
```

**Tests:**
- Default (auto-detect)
- Force NUMA mode
- NUMA disabled
- Half thread count
- Single thread baseline

**Output:** Ranked results with recommended configuration

---

### 2. benchmark_cpu_numa.py - Comprehensive Benchmark (5-10 minutes)

Deep performance analysis with 4 benchmark suites.

```bash
python python/examples/benchmark_cpu_numa.py
```

**Benchmark Suites:**

1. **Thread Scaling** - Test 1,2,4,8,16 threads and all cores
2. **NUMA Modes** - Compare auto/force/disabled
3. **Compression Impact** - Test 1x, 2x, 3x, 5x compression ratios
4. **Optimal Config** - Find best thread count + NUMA mode combination

**Output:**
- Per-suite performance charts (text-based)
- Detailed throughput tables
- Optimal configuration recommendations
- CSV results in `benchmark_results/`

---

### 3. storage_benchmark.py - Storage Performance Testing ⭐ NEW

High-performance storage write benchmark with producer-consumer pipeline.

```bash
# Quick 1 GB test
python python/examples/storage_benchmark.py --size 1GB --output /tmp/test.bin

# NVMe performance test with O_DIRECT
python python/examples/storage_benchmark.py --size 10GB --buffer-size 8MB \
    --output /mnt/nvme/test.bin

# Large test with compression/dedup
python python/examples/storage_benchmark.py --size 100GB \
    --compress-ratio 3.0 --dedup-ratio 2.0 --buffer-size 8MB \
    --output /mnt/nvme/test.bin
```

**Features:**
- Producer-consumer pipeline (generation + writes in parallel)
- O_DIRECT support with automatic fallback
- Page-aligned buffers for optimal NVMe performance
- Detailed metrics: throughput, latency, utilization, bottleneck analysis
- Clear ✓/⚠ indicators for O_DIRECT status
- **Performance**: 0.75-0.80 GB/s (limited by Python overhead, see below)

**Alternative Benchmarks:**
- `single_buffer_benchmark.py` - Achieves 1.26 GB/s by pre-generating all data (requires file size in RAM)
- `iouring_benchmark.py` - Linux io_uring implementation (similar performance to threading version)

**⚠️ Known Limitation**: Streaming mode is limited to 0.75-0.80 GB/s due to Python loop overhead, even though storage can handle 1.26+ GB/s. See [STREAMING_PERFORMANCE_ISSUE.md](STREAMING_PERFORMANCE_ISSUE.md) for analysis and proposed Rust-based solution.

**Output:**
- Real-time progress updates
- Storage throughput (GB/s)
- Per-write latency statistics
- Producer/consumer utilization analysis
- Bottleneck identification

See [STORAGE_BENCHMARK.md](STORAGE_BENCHMARK.md) for detailed documentation and [PERFORMANCE_GUIDE.md](PERFORMANCE_GUIDE.md) for optimization tips.

---

## Example Output

### quick_perf_test.py
```
dgen-py Quick Performance Test
==================================================

System: 1 NUMA node(s), 12 CPUs
  → Single-socket system (UMA)

Running tests...
--------------------------------------------------

1. Default (auto-detect)... 1.05 GB/s
2. Force NUMA... 1.04 GB/s
3. NUMA disabled... 1.08 GB/s
4. Half threads (6)... 1.12 GB/s
5. Single thread (baseline)... 0.73 GB/s

==================================================
RESULTS (fastest to slowest):
==================================================
★ 1. Half threads (6)              1.12 GB/s
  2. NUMA disabled                 1.08 GB/s
  3. Default (auto)                1.05 GB/s

==================================================
RECOMMENDATION: Half threads (6)
  Throughput: 1.12 GB/s
  Code: dgen_py.generate_data(size, max_threads=6)
==================================================
```

---

## System Requirements

- **Python**: 3.8+ (3.12 recommended via uv)
- **dgen-py**: Built from source with `maturin develop --release`
- **OS**: Linux (best performance), macOS, Windows

## Performance Tips

### UMA Systems (Cloud VMs, Workstations)
- Use `numa_mode="disabled"` to avoid detection overhead
- Experiment with thread counts (often half or 3/4 of cores is optimal)
- Single-socket systems won't benefit from NUMA optimizations

### NUMA Systems (Bare Metal, Multi-Socket)
- Use `numa_mode="auto"` (default) for intelligent detection
- Use `numa_mode="force"` to force optimizations
- Expect **30-50% throughput improvement** from thread pinning + first-touch
- Run benchmarks on actual hardware to measure gains

### General
- Always test on your target hardware
- Compression ratio affects optimal thread count
- I/O-bound workloads may benefit from more threads
- CPU-bound workloads may saturate with fewer threads

---

## Interpreting Results

### Throughput (GB/s)
- **< 1 GB/s**: Single thread or small dataset
- **1-5 GB/s**: Good multi-threaded UMA performance
- **5-10 GB/s**: Excellent UMA or good NUMA performance
- **10-20 GB/s**: Excellent NUMA with optimizations

### Scaling Efficiency
```python
efficiency = (throughput_N_threads / throughput_1_thread) / N
```
- **> 0.8**: Excellent scaling (near-linear)
- **0.5-0.8**: Good scaling
- **< 0.5**: Poor scaling (reduce thread count)

### NUMA vs UMA
- **UMA systems**: Force/Auto should show similar performance
- **NUMA systems**: Force should outperform Disabled by 30-50%

---

## Development

To modify these scripts:

```bash
# Edit the scripts
vim python/examples/quick_perf_test.py

# Rebuild if you changed Rust code
maturin develop --release

# Re-run tests
python python/examples/quick_perf_test.py
```

---

## Troubleshooting

### ModuleNotFoundError: No module named 'dgen_py'

Run `maturin develop --release` to build and install the package.

### ImportError: cannot import name 'generate_data'

Your dgen-py installation is outdated. Rebuild:
```bash
maturin develop --release --force
```

### Low Performance on NUMA Systems

1. Verify NUMA detection: `python -c "import dgen_py; print(dgen_py.get_system_info())"`
2. Check `num_nodes` > 1
3. Try `numa_mode="force"`
4. Ensure running on actual NUMA hardware (not VM)

### Performance Varies Between Runs

- Normal variation: ±5%
- Large variation: System is busy, close background apps
- Benchmark script runs warmup iterations to stabilize

---

## Contributing

Found optimal settings for your hardware? Share them!

Create an issue or PR with:
- Hardware specs (CPU, sockets, NUMA topology)
- Benchmark results
- Optimal configuration

This helps us improve auto-detection and recommendations.
