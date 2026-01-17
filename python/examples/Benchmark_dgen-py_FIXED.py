#!/usr/bin/env python3
# Benchmark dgen-py performance - ZERO-COPY PARALLEL STREAMING
# Uses Generator.fill_chunk() with optimized zero-copy implementation

import dgen_py
import time

# Print out NUMA information that dgen-py sees
info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")
    print()


# Test with 100 GB (not 1 TB)
TEST_SIZE = 100 * 1024 * 1024 * 1024  # 100 GB

# Optimal chunk size: 64 MB (16 blocks = good parallelization)
CHUNK_SIZE = 64 * 1024 * 1024  # 64 MB

ITERATIONS = 3  # Only 3 runs
run_times = []

print(f"Starting Benchmark: {ITERATIONS} runs of {TEST_SIZE / (1024**3):.0f} GB each ({CHUNK_SIZE / (1024**2):.0f} MB chunks)")
print("Using ZERO-COPY PARALLEL STREAMING")
print("-" * 60)

for i in range(1, ITERATIONS + 1):
    # Create streaming generator
    gen = dgen_py.Generator(
        size=TEST_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=None  # Use all available cores
    )
    
    # Pre-allocate reusable buffer
    buffer = bytearray(CHUNK_SIZE)
    
    start_time = time.perf_counter()
    
    # Stream through data in parallel chunks (ZERO-COPY)
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
    
    end_time = time.perf_counter()
    duration = end_time - start_time
    throughput = (TEST_SIZE / (1024**3)) / duration  # GB/s
    
    run_times.append(duration)
    print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GB/s")

# Calculate Statistics
avg_duration = sum(run_times) / ITERATIONS
avg_throughput = (TEST_SIZE / (1024**3)) / avg_duration

print("-" * 60)
print(f"AVERAGE DURATION:   {avg_duration:.4f} seconds")
print(f"AVERAGE THROUGHPUT: {avg_throughput:.2f} GB/s")
print(f"PER-CORE THROUGHPUT: {avg_throughput / info['physical_cores']:.2f} GB/s")
print()
print("EXPECTED ON 384-CORE HPC:")
expected_hpc = avg_throughput / info['physical_cores'] * 384
print(f"  Projected throughput: {expected_hpc:.0f} GB/s")
print(f"  Storage target: 80 GB/s")
print(f"  Headroom: {expected_hpc / 80:.1f}x faster than storage")
print()
print("OPTIMIZATION NOTES:")
print("  - Thread pool created ONCE and reused")
print("  - ZERO-COPY: Generates directly into output buffer")
print("  - Each thread works on 4 MB blocks (fits in L3 cache)")
print("  - Parallel generation across all available cores")

