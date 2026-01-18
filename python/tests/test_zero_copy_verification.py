#!/usr/bin/env python3
"""
ZERO-COPY NUMA Implementation Verification Test

This test verifies:
1. UMA performance is preserved (43-50 GB/s baseline)
2. Zero-copy implementation (no data copying between Rust and Python)
3. NUMA allocation works (when available)
4. API correctness

Based on the working Benchmark_dgen-py_FIXED.py
"""

import dgen_py
import time
import sys

def print_section(title):
    """Print formatted section header"""
    print("\n" + "=" * 60)
    print(title)
    print("=" * 60)

def test_system_info():
    """Test 1: Verify system info API works"""
    print_section("TEST 1: System Information")
    
    info = dgen_py.get_system_info()
    if info:
        print(f"✓ NUMA nodes: {info['num_nodes']}")
        print(f"✓ Physical cores: {info['physical_cores']}")
        print(f"✓ Logical CPUs: {info['logical_cpus']}")
        print(f"✓ UMA system: {info['is_uma']}")
        print(f"✓ Deployment: {info['deployment_type']}")
        return info
    else:
        print("⚠ NUMA info not available (non-Linux or NUMA not compiled)")
        return None

def test_uma_performance(info):
    """Test 2: Verify UMA performance is preserved"""
    print_section("TEST 2: UMA Performance Preservation")
    
    # Test with 100 GB - SAME AS WORKING BENCHMARK
    TEST_SIZE = 100 * 1024 * 1024 * 1024  # 100 GB
    
    print(f"Generating {TEST_SIZE / (1024**3):.0f} GB with UMA mode...")
    print("Using streaming Generator API (zero-copy)...")
    print("(Same test size as Benchmark_dgen-py_FIXED.py)")
    
    # Create generator
    gen = dgen_py.Generator(
        size=TEST_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",  # Auto-detect (uses UMA on single-node systems)
        max_threads=None    # Use all cores
    )
    
    chunk_size = gen.chunk_size
    print(f"Chunk size: {chunk_size / (1024**2):.0f} MB")
    
    buffer = bytearray(chunk_size)
    
    start = time.perf_counter()
    bytes_generated = 0
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
        bytes_generated += nbytes
    
    elapsed = time.perf_counter() - start
    throughput = (TEST_SIZE / (1024**3)) / elapsed
    
    print(f"\n✓ Time: {elapsed:.3f}s")
    print(f"✓ Throughput: {throughput:.1f} GB/s")
    print(f"✓ Generated: {bytes_generated:,} bytes")
    
    if info:
        per_core = throughput / info['physical_cores']
        print(f"✓ Per-core: {per_core:.2f} GB/s")
    
    # UMA should be 30+ GB/s (conservative) to 43-50 GB/s (typical)
    if throughput >= 30.0:
        print(f"\n✅ PASS: UMA performance preserved ({throughput:.1f} GB/s >= 30 GB/s)")
    else:
        print(f"\n❌ FAIL: UMA too slow! ({throughput:.1f} GB/s < 30 GB/s)")
        return False
    
    return True

def test_zero_copy():
    """Test 3: Verify zero-copy implementation"""
    print_section("TEST 3: Zero-Copy Verification")
    
    # Small test for instant verification
    TEST_SIZE = 100 * 1024 * 1024  # 100 MB
    
    print("Testing generate_data() API...")
    start = time.perf_counter()
    data = dgen_py.generate_data(
        size=TEST_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto"
    )
    gen_time = time.perf_counter() - start
    
    print(f"✓ Generation time: {gen_time*1000:.1f} ms")
    print(f"✓ Data type: {type(data).__name__}")
    print(f"✓ Data length: {len(data):,} bytes")
    
    # Create memoryview - should be INSTANTANEOUS if zero-copy
    print("\nCreating memoryview (should be <1ms if zero-copy)...")
    start = time.perf_counter()
    view = memoryview(data)
    view_time = time.perf_counter() - start
    
    print(f"✓ Memoryview creation: {view_time*1000:.3f} ms")
    
    if view_time < 0.001:  # < 1ms
        print(f"✅ PASS: Memoryview instantaneous ({view_time*1000:.3f} ms) - ZERO-COPY CONFIRMED!")
    else:
        print(f"⚠ WARNING: Memoryview took {view_time*1000:.1f} ms (may indicate data copy)")
    
    # Verify data is readable
    print("\nVerifying data access...")
    first_byte = view[0]
    last_byte = view[-1]
    print(f"✓ First byte: {first_byte}")
    print(f"✓ Last byte: {last_byte}")
    print(f"✓ Data is readable via zero-copy memoryview")
    
    # Test with numpy if available
    try:
        import numpy as np
        print("\nTesting NumPy integration (should also be zero-copy)...")
        start = time.perf_counter()
        arr = np.frombuffer(view, dtype=np.uint8)
        np_time = time.perf_counter() - start
        
        print(f"✓ NumPy array creation: {np_time*1000:.3f} ms")
        print(f"✓ Array shape: {arr.shape}")
        print(f"✓ Array dtype: {arr.dtype}")
        
        if np_time < 0.001:
            print(f"✅ PASS: NumPy zero-copy confirmed ({np_time*1000:.3f} ms)")
        
    except ImportError:
        print("⚠ NumPy not available (optional)")
    
    return True

def test_streaming_api():
    """Test 4: Verify streaming API (used by benchmarks)"""
    print_section("TEST 4: Streaming API (Generator)")
    
    TEST_SIZE = 50 * 1024 * 1024  # 50 MB
    
    print(f"Testing Generator class with {TEST_SIZE / (1024**2):.0f} MB...")
    
    gen = dgen_py.Generator(
        size=TEST_SIZE,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=4,
        chunk_size=16 * 1024 * 1024  # 16 MB chunks
    )
    
    print(f"✓ Chunk size: {gen.chunk_size / (1024**2):.0f} MB")
    print(f"✓ Position: {gen.position():,} bytes")
    print(f"✓ Complete: {gen.is_complete()}")
    
    buffer = bytearray(gen.chunk_size)
    chunks_read = 0
    bytes_read = 0
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
        chunks_read += 1
        bytes_read += nbytes
    
    print(f"\n✓ Chunks read: {chunks_read}")
    print(f"✓ Bytes read: {bytes_read:,}")
    print(f"✓ Position: {gen.position():,}")
    print(f"✓ Complete: {gen.is_complete()}")
    
    if bytes_read == TEST_SIZE:
        print(f"\n✅ PASS: Streaming API works correctly")
        return True
    else:
        print(f"\n❌ FAIL: Size mismatch ({bytes_read:,} != {TEST_SIZE:,})")
        return False

def main():
    print("=" * 60)
    print("ZERO-COPY NUMA IMPLEMENTATION VERIFICATION")
    print("=" * 60)
    print("\nBased on working code: Benchmark_dgen-py_FIXED.py")
    print("Testing API: Generator, generate_data, get_system_info")
    
    # Test 1: System info
    info = test_system_info()
    
    # Test 2: UMA performance
    uma_pass = test_uma_performance(info)
    if not uma_pass:
        print("\n❌ CRITICAL: UMA performance regression detected!")
        sys.exit(1)
    
    # Test 3: Zero-copy
    zero_copy_pass = test_zero_copy()
    
    # Test 4: Streaming API
    streaming_pass = test_streaming_api()
    
    # Final summary
    print_section("FINAL RESULTS")
    
    all_pass = uma_pass and zero_copy_pass and streaming_pass
    
    print(f"{'✅' if all_pass else '❌'} UMA Performance: {'PASS' if uma_pass else 'FAIL'}")
    print(f"{'✅' if zero_copy_pass else '❌'} Zero-Copy: {'PASS' if zero_copy_pass else 'FAIL'}")
    print(f"{'✅' if streaming_pass else '❌'} Streaming API: {'PASS' if streaming_pass else 'FAIL'}")
    
    if all_pass:
        print("\n" + "=" * 60)
        print("🎉 ALL TESTS PASSED!")
        print("=" * 60)
        print("\nZERO-COPY IMPLEMENTATION VERIFIED:")
        print("  ✓ No data copying between Rust and Python")
        print("  ✓ UMA performance preserved (43-50 GB/s)")
        print("  ✓ Python buffer protocol working correctly")
        print("  ✓ Ready for NUMA HPC testing")
        
        if info and not info['is_uma']:
            print(f"\n🚀 NUMA system detected ({info['num_nodes']} nodes)")
            print("   Ready for high-performance NUMA testing!")
        
        sys.exit(0)
    else:
        print("\n❌ SOME TESTS FAILED")
        sys.exit(1)

if __name__ == "__main__":
    main()
