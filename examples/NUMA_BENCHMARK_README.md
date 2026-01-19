# NUMA Optimization Benchmark

This Rust benchmark tests various NUMA configurations to find optimal settings for your system.

## Building

```bash
cargo build --release --example numa-bench
```

The binary will be at: `./target/release/examples/numa-bench`

## Running on Remote Server

1. **Copy the binary to your server:**
   ```bash
   scp ./target/release/examples/numa-bench user@server:~/
   ```

2. **Run the benchmark:**
   ```bash
   ssh user@server
   ./numa-bench
   ```

3. **For detailed logs, enable tracing:**
   ```bash
   RUST_LOG=info ./numa-bench
   ```

## What It Tests

The benchmark runs 10 different configurations, each with 3 iterations:

1. **Baseline** - All cores, NUMA auto, 32 MB chunks
2. **Physical Cores Only** - Disable hyperthreading
3. **Force NUMA** - Force NUMA optimizations
4. **NUMA Disabled** - No thread pinning
5. **16 MB Chunks** - Smaller buffer size
6. **64 MB Chunks** - Larger buffer size
7. **512 KB Blocks** - Finer parallelization granularity
8. **2 MB Blocks** - Coarser parallelization granularity
9. **Multi-Socket Optimized** - Force NUMA + physical cores
10. **Half Cores** - Test scaling behavior

Each test generates 100 GB of data and reports:
- Average throughput (GB/s)
- Duration (seconds)
- Percentage of best configuration

## Expected Output

```
======================================================================
NUMA OPTIMIZATION BENCHMARK
======================================================================
Test size: 100 GB
Iterations: 3
======================================================================

System detected:
  Total CPUs: 96 (logical)
  Physical cores: 48

TEST 1: Baseline (all cores, NUMA auto, 32 MB chunks)
  [Baseline] Iteration 1: 85.32 GB/s (1.17s)
  [Baseline] Iteration 2: 86.45 GB/s (1.16s)
  [Baseline] Iteration 3: 85.90 GB/s (1.16s)

...

======================================================================
RESULTS SUMMARY
======================================================================
Configuration                  |      Throughput |        Duration |  % of Best
----------------------------------------------------------------------
Baseline                       |        85.89 GB/s |         1.16s |      100.0% ← BEST
Physical Cores Only            |        78.62 GB/s |         1.27s |       91.5%
...

🏆 OPTIMAL CONFIGURATION: Baseline
   Throughput: 85.89 GB/s
   Duration: 1.16s for 100 GB
   Per physical core: 1.79 GB/s

📋 RECOMMENDATIONS:
   For single-process workloads: Use the best configuration above
   For multi-process NUMA workloads: Run multiple processes with:
     - Set CPU affinity per NUMA node (os.sched_setaffinity)
     - Use numa_node parameter to bind memory allocation
     - Each process uses physical cores from its NUMA node
```

## Interpreting Results

### Per-Core Throughput
- **UMA (single socket):** Expect 4-8 GB/s per physical core
- **Multi-socket NUMA:** Expect 2-4 GB/s per physical core (memory bandwidth shared per socket)

### NUMA Mode
- **Auto:** Enables optimizations only on detected multi-socket systems
- **Force:** Always enables optimizations (good for testing)
- **Disabled:** No thread pinning (useful for comparison)

### Block Size
- **Smaller blocks (512 KB):** Better parallelization, more overhead
- **Larger blocks (2 MB):** Less overhead, potential load imbalance
- **Default (1 MB):** Good balance for most workloads

### Hyperthreading
- Memory-intensive workloads often perform **better with hyperthreading disabled**
- Test both configurations to find what works best for your hardware

## Using Results with Python

Once you identify the optimal configuration, update your Python benchmark:

```python
# If "Force NUMA + physical cores" wins:
gen = dgen_py.Generator(
    size=total_size,
    numa_mode="force",
    max_threads=24,  # Physical cores for this NUMA node
    numa_node=node_id,
    # ... other params
)
```

## Multi-Process NUMA Usage

The benchmark tests **single-process** performance. For multi-socket systems:

1. Run **one Python process per NUMA node**
2. **Pin each process** to its NUMA node's CPUs with `os.sched_setaffinity()`
3. Set `numa_node=N` to bind memory allocation
4. Use the optimal configuration from this benchmark

Example: 2-socket system with 24 cores/socket
- Process 0: CPUs 0-23 (or 0-47 with hyperthreading), `numa_node=0`
- Process 1: CPUs 24-47 (or 48-95 with hyperthreading), `numa_node=1`

See `python/examples/benchmark_numa_multiprocess_FIXED.py` for complete implementation.
