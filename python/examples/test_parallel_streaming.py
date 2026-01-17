#!/usr/bin/env python3
"""
Test parallel streaming with Generator.fill_chunk()

This demonstrates that fill_chunk() now uses parallel generation
when filling buffers >= 8 MB (2 blocks).
"""

import dgen_py
import time

print("Testing Parallel Streaming Generator")
print("=" * 60)

# Get system info
info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")
    print()

# Test configuration
TOTAL_SIZE = 1024 * 1024 * 1024  # 1 GB
CHUNK_SIZES = [
    (4 * 1024 * 1024, "4 MB (1 block - sequential)"),
    (8 * 1024 * 1024, "8 MB (2 blocks - parallel)"),
    (64 * 1024 * 1024, "64 MB (16 blocks - parallel)"),
    (256 * 1024 * 1024, "256 MB (64 blocks - parallel)"),
]

print(f"Generating {TOTAL_SIZE / (1024**3):.1f} GB total")
print("Testing different chunk sizes:")
print()

for chunk_size, description in CHUNK_SIZES:
    # Create generator with explicit thread configuration
    gen = dgen_py.Generator(
        size=TOTAL_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=None  # Use all available cores
    )
    
    # Pre-allocate buffer
    buffer = bytearray(chunk_size)
    
    # Generate all data
    start_time = time.perf_counter()
    total_bytes = 0
    chunks = 0
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
        total_bytes += nbytes
        chunks += 1
    
    elapsed = time.perf_counter() - start_time
    throughput = (total_bytes / (1024**3)) / elapsed
    
    print(f"{description:40} {throughput:6.2f} GB/s  ({elapsed:.3f}s, {chunks} chunks)")

print()
print("=" * 60)
print("RESULTS:")
print("  - Small chunks (4 MB) = sequential (single-threaded)")
print("  - Large chunks (≥8 MB) = parallel (multi-threaded)")
print()
print("Expected: Larger chunks should be MUCH faster on multi-core systems")
print("=" * 60)
