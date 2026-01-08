#!/usr/bin/env python3
"""
Performance Benchmark: CPU and NUMA Configuration
==================================================

This script helps you find the optimal CPU and NUMA settings for your system.
Run this to discover:
- Optimal thread count for your workload
- Whether NUMA optimizations help (bare metal) or hurt (cloud VM)
- Baseline single-core performance
- Memory bandwidth limits

Usage:
    python benchmark_cpu_numa.py

Requirements:
    pip install dgen-py
"""

import time
import sys
import os
from typing import Optional, List, Tuple

try:
    import dgen_py
except ImportError:
    print("ERROR: dgen-py not installed")
    print("Install with: pip install dgen-py")
    print("Or build from source: cd dgen-rs && maturin develop --release")
    sys.exit(1)


class PerformanceBenchmark:
    """Comprehensive CPU and NUMA performance benchmark"""
    
    def __init__(self, size_mb: int = 100):
        self.size = size_mb * 1024 * 1024
        self.results = []
        
    def run_test(
        self, 
        name: str, 
        dedup_ratio: float = 1.0,
        compress_ratio: float = 1.0,
        numa_mode: str = "auto",
        max_threads: Optional[int] = None,
        iterations: int = 3
    ) -> Tuple[float, float]:
        """Run a single test configuration multiple times and return avg throughput"""
        
        times = []
        for i in range(iterations):
            start = time.perf_counter()
            data = dgen_py.generate_data(
                self.size,
                dedup_ratio=dedup_ratio,
                compress_ratio=compress_ratio,
                numa_mode=numa_mode,
                max_threads=max_threads
            )
            elapsed = time.perf_counter() - start
            times.append(elapsed)
            
            # Verify size
            if len(data) != self.size:
                print(f"WARNING: Expected {self.size} bytes, got {len(data)}")
        
        avg_time = sum(times) / len(times)
        throughput = (self.size / avg_time) / 1e9  # GB/s
        
        self.results.append({
            'name': name,
            'throughput': throughput,
            'time': avg_time,
            'threads': max_threads or 'all',
            'numa_mode': numa_mode,
            'dedup': dedup_ratio,
            'compress': compress_ratio
        })
        
        return throughput, avg_time
    
    def print_result(self, name: str, throughput: float, time_sec: float, 
                     threads: Optional[int] = None, numa_mode: str = "auto"):
        """Print a single test result"""
        threads_str = f"{threads} threads" if threads else "all cores"
        print(f"  {name:40s} {throughput:8.2f} GB/s  ({time_sec:.3f}s, {threads_str}, numa={numa_mode})")
    
    def benchmark_thread_scaling(self):
        """Test performance with different thread counts"""
        print("\n" + "="*80)
        print("THREAD SCALING BENCHMARK")
        print("="*80)
        print("\nTesting different thread counts to find optimal configuration...")
        print("(All tests: incompressible data, no dedup, NUMA=auto)\n")
        
        # Get CPU count for intelligent thread selection
        import multiprocessing
        cpu_count = multiprocessing.cpu_count()
        
        thread_counts = [1, 2, 4]
        if cpu_count >= 8:
            thread_counts.append(8)
        if cpu_count >= 16:
            thread_counts.append(16)
        thread_counts.append(None)  # All cores
        
        baseline_throughput = None
        
        for threads in thread_counts:
            name = f"Threads: {threads if threads else cpu_count} ({'baseline' if threads == 1 else 'parallel'})"
            throughput, elapsed = self.run_test(name, max_threads=threads)
            self.print_result(name, throughput, elapsed, threads)
            
            if threads == 1:
                baseline_throughput = throughput
            elif baseline_throughput:
                speedup = throughput / baseline_throughput
                efficiency = (speedup / (threads or cpu_count)) * 100
                print(f"    └─> Speedup: {speedup:.2f}x, Efficiency: {efficiency:.1f}%")
    
    def benchmark_numa_modes(self):
        """Test different NUMA configurations"""
        print("\n" + "="*80)
        print("NUMA MODE BENCHMARK")
        print("="*80)
        print("\nTesting NUMA optimization modes...")
        print("(All tests: incompressible data, no dedup, all cores)\n")
        
        # Check system NUMA topology
        numa_info = dgen_py.get_system_info()
        if numa_info:
            num_nodes = numa_info['num_nodes']
            print(f"System: {num_nodes} NUMA node(s) detected")
            if num_nodes > 1:
                print(f"  Multi-socket NUMA system - optimizations should help!")
            else:
                print(f"  UMA system (single socket) - optimizations add minimal overhead")
            print()
        else:
            print("NUMA detection not available (NUMA feature not compiled in)\n")
        
        modes = [
            ("Auto (default)", "auto"),
            ("Force NUMA", "force"),
            ("Disabled", "disabled")
        ]
        
        for name, mode in modes:
            test_name = f"NUMA mode: {name}"
            throughput, elapsed = self.run_test(test_name, numa_mode=mode)
            self.print_result(test_name, throughput, elapsed, numa_mode=mode)
    
    def benchmark_compression_impact(self):
        """Test performance with different compression ratios"""
        print("\n" + "="*80)
        print("COMPRESSION IMPACT BENCHMARK")
        print("="*80)
        print("\nTesting how compression ratio affects throughput...")
        print("(All tests: no dedup, NUMA=auto, all cores)\n")
        
        compress_ratios = [1, 2, 3, 5]
        
        baseline = None
        for ratio in compress_ratios:
            name = f"Compression ratio: {ratio}:1 ({'incompressible' if ratio == 1 else 'compressible'})"
            throughput, elapsed = self.run_test(name, compress_ratio=ratio)
            self.print_result(name, throughput, elapsed)
            
            if ratio == 1:
                baseline = throughput
            else:
                slowdown = baseline / throughput if throughput > 0 else 0
                print(f"    └─> {slowdown:.2f}x slower than incompressible (more back-refs to copy)")
    
    def benchmark_optimal_config(self):
        """Find optimal configuration for this system"""
        print("\n" + "="*80)
        print("OPTIMAL CONFIGURATION FINDER")
        print("="*80)
        print("\nTesting combinations to find best performance...\n")
        
        import multiprocessing
        cpu_count = multiprocessing.cpu_count()
        
        configs = [
            ("All cores + Auto NUMA", None, "auto"),
            ("All cores + Force NUMA", None, "force"),
            ("All cores + Disabled NUMA", None, "disabled"),
            ("Half cores + Auto NUMA", cpu_count // 2, "auto"),
        ]
        
        best_throughput = 0
        best_config = None
        
        for name, threads, numa_mode in configs:
            test_name = f"Config: {name}"
            throughput, elapsed = self.run_test(test_name, max_threads=threads, numa_mode=numa_mode)
            self.print_result(test_name, throughput, elapsed, threads, numa_mode)
            
            if throughput > best_throughput:
                best_throughput = throughput
                best_config = (name, threads, numa_mode)
        
        print(f"\n{'='*80}")
        print(f"WINNER: {best_config[0]}")
        print(f"  Throughput: {best_throughput:.2f} GB/s")
        print(f"  Config: max_threads={best_config[1]}, numa_mode='{best_config[2]}'")
        print(f"{'='*80}")
    
    def print_summary(self):
        """Print overall benchmark summary"""
        print("\n" + "="*80)
        print("BENCHMARK SUMMARY")
        print("="*80)
        
        if not self.results:
            print("No results to summarize")
            return
        
        # Sort by throughput
        sorted_results = sorted(self.results, key=lambda x: x['throughput'], reverse=True)
        
        print(f"\nTop 5 configurations (out of {len(self.results)} tested):\n")
        print(f"{'Rank':<6} {'Configuration':<40} {'Throughput':<12} {'Settings'}")
        print("-" * 80)
        
        for i, result in enumerate(sorted_results[:5], 1):
            config = result['name']
            throughput = f"{result['throughput']:.2f} GB/s"
            settings = f"threads={result['threads']}, numa={result['numa_mode']}"
            print(f"{i:<6} {config:<40} {throughput:<12} {settings}")
        
        # System recommendations
        print(f"\n{'='*80}")
        print("RECOMMENDATIONS FOR YOUR SYSTEM")
        print(f"{'='*80}\n")
        
        best = sorted_results[0]
        print(f"For maximum performance on this system:")
        print(f"  dgen_py.generate_data(size,")
        print(f"      max_threads={repr(best['threads'])},")
        print(f"      numa_mode='{best['numa_mode']}')")
        print(f"  Expected: ~{best['throughput']:.1f} GB/s")
        
        # Check if NUMA helps
        numa_info = dgen_py.get_system_info()
        if numa_info and numa_info['num_nodes'] == 1:
            print("\nNOTE: This is a UMA system (single NUMA node).")
            print("  NUMA optimizations have minimal impact here.")
            print("  On multi-socket bare metal servers, expect 30-50% improvement!")


def main():
    """Run comprehensive benchmark suite"""
    
    print("""
╔══════════════════════════════════════════════════════════════════════════════╗
║                  dgen-py Performance Benchmark Suite                         ║
║                  CPU and NUMA Configuration Optimizer                        ║
╚══════════════════════════════════════════════════════════════════════════════╝
    """)
    
    # Check system info
    numa_info = dgen_py.get_system_info()
    if numa_info:
        print(f"System Information:")
        print(f"  NUMA nodes: {numa_info['num_nodes']}")
        print(f"  Physical cores: {numa_info['physical_cores']}")
        print(f"  Logical CPUs: {numa_info['logical_cpus']}")
        print(f"  UMA system: {numa_info['is_uma']}")
        print(f"  Deployment: {numa_info['deployment_type']}")
    else:
        import multiprocessing
        print(f"System Information:")
        print(f"  CPUs: {multiprocessing.cpu_count()}")
        print(f"  NUMA: Not available (feature not compiled in)")
    
    print(f"\nBenchmark Configuration:")
    print(f"  Data size per test: 100 MiB")
    print(f"  Iterations per config: 3")
    print(f"  Total time: ~1-2 minutes")
    
    # Run benchmarks
    benchmark = PerformanceBenchmark(size_mb=100)
    
    benchmark.benchmark_thread_scaling()
    benchmark.benchmark_numa_modes()
    benchmark.benchmark_compression_impact()
    benchmark.benchmark_optimal_config()
    benchmark.print_summary()
    
    print(f"\n{'='*80}")
    print("Benchmark complete! Save these results for your production configuration.")
    print(f"{'='*80}\n")


if __name__ == "__main__":
    main()
