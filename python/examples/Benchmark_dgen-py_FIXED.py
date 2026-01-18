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

ITERATIONS = 3  # Only 3 runs

print(f"Starting Benchmark: {ITERATIONS} runs of {TEST_SIZE / (1024**3):.0f} GB each")
print("Using ZERO-COPY PARALLEL STREAMING")
print()

# Test 1: Default chunk size (should be 32 MB)
print("=" * 60)
print("TEST 1: DEFAULT CHUNK SIZE (should use optimal 32 MB)")
print("=" * 60)
run_times = []

for i in range(1, ITERATIONS + 1):
    # Create streaming generator WITHOUT specifying chunk_size
    gen = dgen_py.Generator(
        size=TEST_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=None  # Use all available cores
        # chunk_size NOT specified - should default to 32 MB
    )
    
    # Get the actual chunk size being used
    chunk_size = gen.chunk_size
    buffer = bytearray(chunk_size)
    
    if i == 1:
        print(f"Using chunk size: {chunk_size / (1024**2):.0f} MB")
        print("-" * 60)
    
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

# Test 2: Override with 64 MB chunk size
print("=" * 60)
print("TEST 2: OVERRIDE CHUNK SIZE TO 64 MB")
print("=" * 60)
run_times_64 = []

for i in range(1, ITERATIONS + 1):
    # Create streaming generator WITH 64 MB chunk size override
    gen = dgen_py.Generator(
        size=TEST_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=None,
        chunk_size=64 * 1024 * 1024  # Override to 64 MB
    )
    
    chunk_size = gen.chunk_size
    buffer = bytearray(chunk_size)
    
    if i == 1:
        print(f"Using chunk size: {chunk_size / (1024**2):.0f} MB")
        print("-" * 60)
    
    start_time = time.perf_counter()
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
    
    end_time = time.perf_counter()
    duration = end_time - start_time
    throughput = (TEST_SIZE / (1024**3)) / duration  # GB/s
    
    run_times_64.append(duration)
    print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GB/s")

avg_duration_64 = sum(run_times_64) / ITERATIONS
avg_throughput_64 = (TEST_SIZE / (1024**3)) / avg_duration_64

print("-" * 60)
print(f"AVERAGE DURATION:   {avg_duration_64:.4f} seconds")
print(f"AVERAGE THROUGHPUT: {avg_throughput_64:.2f} GB/s")
print(f"PER-CORE THROUGHPUT: {avg_throughput_64 / info['physical_cores']:.2f} GB/s")
print()

# Comparison
print("=" * 60)
print("COMPARISON")
print("=" * 60)
print(f"32 MB (default): {avg_throughput:.2f} GB/s")
print(f"64 MB (override): {avg_throughput_64:.2f} GB/s")
improvement = ((avg_throughput - avg_throughput_64) / avg_throughput_64) * 100
if improvement > 0:
    print(f"32 MB is {improvement:.1f}% faster than 64 MB")
else:
    print(f"64 MB is {-improvement:.1f}% faster than 32 MB")
print()

print("OPTIMIZATION NOTES:")
print("  - Thread pool created ONCE and reused")
print("  - ZERO-COPY: Generates directly into output buffer")
print("  - Internal parallelization: 4 MiB blocks (optimal for L3 cache)")
print("  - Parallel generation distributes blocks across all available cores")

