#!/usr/bin/env python3
"""
Quick Performance Test
======================

Fast 30-second test to find optimal settings for your system.

Usage:
    python quick_perf_test.py
"""

import time
import sys

try:
    import dgen_py
except ImportError:
    print("ERROR: dgen-py not installed")
    print("Run: pip install dgen-py")
    sys.exit(1)


def test_config(name, size, **kwargs):
    """Test a configuration and return throughput"""
    start = time.perf_counter()
    data = dgen_py.generate_data(size, **kwargs)
    elapsed = time.perf_counter() - start
    throughput = (size / elapsed) / 1e9
    return throughput, elapsed


def main():
    print("dgen-py Quick Performance Test")
    print("=" * 50)
    
    # System info
    info = dgen_py.get_system_info()
    if info:
        print(f"\nSystem: {info['num_nodes']} NUMA node(s), {info['logical_cpus']} CPUs")
        if info['num_nodes'] > 1:
            print("  → Multi-socket system (NUMA optimizations should help!)")
        else:
            print("  → Single-socket system (UMA)")
    
    size = 100 * 1024 * 1024  # 100 MiB
    print(f"\nTest size: 100 MiB per run")
    print(f"\nRunning tests...")
    print("-" * 50)
    
    results = []
    
    # Test 1: Default (auto-detect everything)
    print("\n1. Default (auto-detect)...", end=" ", flush=True)
    tp, t = test_config("default", size)
    print(f"{tp:.2f} GB/s")
    results.append(("Default (auto)", tp, {}))
    
    # Test 2: Force NUMA
    print("2. Force NUMA...", end=" ", flush=True)
    tp, t = test_config("force_numa", size, numa_mode="force")
    print(f"{tp:.2f} GB/s")
    results.append(("Force NUMA", tp, {"numa_mode": "force"}))
    
    # Test 3: NUMA disabled
    print("3. NUMA disabled...", end=" ", flush=True)
    tp, t = test_config("disabled_numa", size, numa_mode="disabled")
    print(f"{tp:.2f} GB/s")
    results.append(("NUMA disabled", tp, {"numa_mode": "disabled"}))
    
    # Test 4: Half threads
    import multiprocessing
    half_threads = multiprocessing.cpu_count() // 2
    print(f"4. Half threads ({half_threads})...", end=" ", flush=True)
    tp, t = test_config("half_threads", size, max_threads=half_threads)
    print(f"{tp:.2f} GB/s")
    results.append((f"Half threads ({half_threads})", tp, {"max_threads": half_threads}))
    
    # Test 5: Single thread (baseline)
    print("5. Single thread (baseline)...", end=" ", flush=True)
    tp, t = test_config("single", size, max_threads=1)
    print(f"{tp:.2f} GB/s")
    results.append(("Single thread", tp, {"max_threads": 1}))
    
    # Find best
    results.sort(key=lambda x: x[1], reverse=True)
    
    print("\n" + "=" * 50)
    print("RESULTS (fastest to slowest):")
    print("=" * 50)
    for i, (name, tp, config) in enumerate(results, 1):
        star = "★" if i == 1 else " "
        print(f"{star} {i}. {name:25s} {tp:8.2f} GB/s")
    
    # Recommendation
    best_name, best_tp, best_config = results[0]
    print("\n" + "=" * 50)
    print(f"RECOMMENDATION: {best_name}")
    print(f"  Throughput: {best_tp:.2f} GB/s")
    if best_config:
        print(f"  Code: dgen_py.generate_data(size, {', '.join(f'{k}={repr(v)}' for k, v in best_config.items())})")
    else:
        print(f"  Code: dgen_py.generate_data(size)  # defaults are optimal!")
    print("=" * 50)


if __name__ == "__main__":
    main()
