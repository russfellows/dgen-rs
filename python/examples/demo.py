#!/usr/bin/env python3
"""
Example: Generate data and compress it to verify compression ratio
"""

import dgen_py
import sys


def test_compression_ratio():
    """Generate data with specific compression ratio and verify"""
    try:
        import zstandard as zstd
    except ImportError:
        print("Error: zstandard package required")
        print("Install with: pip install zstandard")
        sys.exit(1)

    size = 100 * 1024 * 1024  # 100 MiB
    target_compress_ratio = 3.0  # 3:1 compression

    print(f"Generating {size / (1024**2):.1f} MiB with {target_compress_ratio}:1 compression ratio...")
    
    data = dgen_py.generate_data(size, compress_ratio=target_compress_ratio)
    
    print(f"Generated {len(data) / (1024**2):.1f} MiB")
    print("Compressing with zstd...")
    
    compressor = zstd.ZstdCompressor(level=3)
    compressed = compressor.compress(data)
    
    actual_ratio = len(data) / len(compressed)
    
    print(f"\nResults:")
    print(f"  Original:   {len(data) / (1024**2):.2f} MiB")
    print(f"  Compressed: {len(compressed) / (1024**2):.2f} MiB")
    print(f"  Ratio:      {actual_ratio:.2f}:1")
    print(f"  Target:     {target_compress_ratio:.2f}:1")
    print(f"  Delta:      {abs(actual_ratio - target_compress_ratio) / target_compress_ratio * 100:.1f}%")


def test_streaming():
    """Generate large dataset using streaming API"""
    size = 1024 * 1024 * 1024  # 1 GiB
    chunk_size = 4 * 1024 * 1024  # 4 MiB chunks

    print(f"\nGenerating {size / (1024**3):.1f} GiB using streaming API...")
    
    gen = dgen_py.Generator(
        size=size,
        dedup_ratio=2.0,
        compress_ratio=2.0,
        numa_mode="auto"
    )
    
    buf = bytearray(chunk_size)
    total = 0
    chunks = 0
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buf)
        if nbytes == 0:
            break
        
        total += nbytes
        chunks += 1
        
        if chunks % 100 == 0:
            pct = (total / size) * 100
            print(f"  Progress: {total / (1024**3):.2f} GiB ({pct:.1f}%)")
    
    print(f"\nCompleted:")
    print(f"  Total: {total / (1024**3):.3f} GiB")
    print(f"  Chunks: {chunks}")
    print(f"  Avg chunk size: {total / chunks / (1024**2):.2f} MiB")


def show_system_info():
    """Display NUMA topology information"""
    print("\nSystem Information:")
    print("-" * 50)
    
    info = dgen_py.get_system_info()
    if info:
        print(f"  NUMA nodes:      {info['num_nodes']}")
        print(f"  Physical cores:  {info['physical_cores']}")
        print(f"  Logical CPUs:    {info['logical_cpus']}")
        print(f"  UMA system:      {info['is_uma']}")
        print(f"  Deployment:      {info['deployment_type']}")
    else:
        print("  NUMA info not available on this platform")


if __name__ == '__main__':
    show_system_info()
    
    print("\n" + "=" * 50)
    print("Test 1: Compression Ratio Validation")
    print("=" * 50)
    test_compression_ratio()
    
    print("\n" + "=" * 50)
    print("Test 2: Streaming Generation")
    print("=" * 50)
    test_streaming()
    
    print("\n✓ All tests completed!")
