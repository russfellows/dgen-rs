#!/usr/bin/env python3
import dgen_py
import time

# Test different chunk sizes
TEST_SIZE = 10 * 1024**3  # 10 GB
WARMUP_SIZE = 1 * 1024**3  # 1 GB

for chunk_size_mb in [4, 8, 16, 32, 64, 128]:
    chunk_size = chunk_size_mb * 1024 * 1024
    
    # Warmup
    gen = dgen_py.Generator(size=WARMUP_SIZE, chunk_size=chunk_size)
    buf = bytearray(chunk_size)
    while not gen.is_complete():
        gen.fill_chunk(buf)
    
    # Benchmark
    gen = dgen_py.Generator(size=TEST_SIZE, chunk_size=chunk_size)
    buf = bytearray(chunk_size)
    
    start = time.perf_counter()
    while not gen.is_complete():
        gen.fill_chunk(buf)
    end = time.perf_counter()
    
    duration = end - start
    throughput = (TEST_SIZE / 1024**3) / duration
    print(f"{chunk_size_mb:3d} MB chunks: {duration:.4f} seconds | {throughput:.2f} GB/s")
