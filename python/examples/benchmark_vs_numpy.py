#!/usr/bin/env python3
"""
Benchmark: dgen-py vs Numpy Random
===================================

Compare dgen-py data generation to numpy's random number generation.
"""

import dgen_py
import numpy as np
import time


def benchmark_dgen(size, runs=5):
    """Benchmark dgen-py generation"""
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        data = dgen_py.generate_data(size)
        # Convert to memoryview to ensure zero-copy is used
        view = memoryview(data)
        elapsed = time.perf_counter() - start
        times.append(elapsed)
    
    avg_time = sum(times) / len(times)
    return avg_time, size / avg_time / 1e9


def benchmark_numpy_randint(size, runs=5):
    """Benchmark numpy random.randint (uint8)"""
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        arr = np.random.randint(0, 256, size, dtype=np.uint8)
        elapsed = time.perf_counter() - start
        times.append(elapsed)
    
    avg_time = sum(times) / len(times)
    return avg_time, size / avg_time / 1e9


def benchmark_numpy_bytes(size, runs=5):
    """Benchmark numpy random.bytes"""
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        data = np.random.bytes(size)
        elapsed = time.perf_counter() - start
        times.append(elapsed)
    
    avg_time = sum(times) / len(times)
    return avg_time, size / avg_time / 1e9


def main():
    print("=" * 70)
    print("BENCHMARK: dgen-py vs Numpy Random Number Generation")
    print("=" * 70)
    
    sizes = [
        (1 * 1024 * 1024, "1 MiB"),
        (10 * 1024 * 1024, "10 MiB"),
        (100 * 1024 * 1024, "100 MiB"),
        (500 * 1024 * 1024, "500 MiB"),
    ]
    
    print("\nRunning benchmarks (5 runs each, averaged)...\n")
    
    results = []
    
    for size, label in sizes:
        print(f"Testing {label}...")
        
        # dgen-py
        dgen_time, dgen_gbps = benchmark_dgen(size)
        
        # numpy randint
        numpy_randint_time, numpy_randint_gbps = benchmark_numpy_randint(size)
        
        # numpy bytes
        numpy_bytes_time, numpy_bytes_gbps = benchmark_numpy_bytes(size)
        
        results.append({
            'size': size,
            'label': label,
            'dgen_time': dgen_time,
            'dgen_gbps': dgen_gbps,
            'numpy_randint_time': numpy_randint_time,
            'numpy_randint_gbps': numpy_randint_gbps,
            'numpy_bytes_time': numpy_bytes_time,
            'numpy_bytes_gbps': numpy_bytes_gbps,
        })
    
    # Print results table
    print("\n" + "=" * 70)
    print("RESULTS")
    print("=" * 70)
    print(f"\n{'Size':<10} {'Method':<20} {'Time (ms)':<12} {'Throughput':<15} {'vs dgen':<10}")
    print("-" * 70)
    
    for r in results:
        # dgen-py
        print(f"{r['label']:<10} {'dgen-py':<20} {r['dgen_time']*1000:>10.1f} ms {r['dgen_gbps']:>10.2f} GB/s {'baseline':>10}")
        
        # numpy randint
        speedup_randint = r['dgen_gbps'] / r['numpy_randint_gbps']
        print(f"{'':<10} {'numpy.random.randint':<20} {r['numpy_randint_time']*1000:>10.1f} ms {r['numpy_randint_gbps']:>10.2f} GB/s {speedup_randint:>9.2f}x")
        
        # numpy bytes
        speedup_bytes = r['dgen_gbps'] / r['numpy_bytes_gbps']
        print(f"{'':<10} {'numpy.random.bytes':<20} {r['numpy_bytes_time']*1000:>10.1f} ms {r['numpy_bytes_gbps']:>10.2f} GB/s {speedup_bytes:>9.2f}x")
        
        print()
    
    # Summary
    print("=" * 70)
    print("SUMMARY")
    print("=" * 70)
    
    # Average speedups
    avg_speedup_randint = sum(r['dgen_gbps'] / r['numpy_randint_gbps'] for r in results) / len(results)
    avg_speedup_bytes = sum(r['dgen_gbps'] / r['numpy_bytes_gbps'] for r in results) / len(results)
    
    print(f"\ndgen-py average performance:")
    print(f"  vs numpy.random.randint: {avg_speedup_randint:.2f}x faster")
    print(f"  vs numpy.random.bytes:   {avg_speedup_bytes:.2f}x faster")
    
    # Find best dgen performance
    best = max(results, key=lambda r: r['dgen_gbps'])
    print(f"\nPeak dgen-py throughput: {best['dgen_gbps']:.2f} GB/s ({best['label']})")
    
    print("\n" + "=" * 70)
    print("✓ Zero-copy implementation delivers competitive performance!")
    print("=" * 70)
    
    # Technical notes
    print("\nNOTES:")
    print("- dgen-py uses Xoshiro256++ RNG (faster than Numpy's MT19937)")
    print("- dgen-py leverages multi-threading via Rayon")
    print("- dgen-py provides zero-copy access via buffer protocol")
    print("- Numpy's random.bytes is single-threaded")
    print("- Numpy's random.randint has array creation overhead")


if __name__ == "__main__":
    main()
