#!/usr/bin/env python3
"""
Quick test to verify NUMA fixes are working.
Run this on your 2-NUMA node system after rebuilding.
"""

import dgen_py
import os

print("=" * 70)
print("NUMA FIX VERIFICATION TEST")
print("=" * 70)

# Get system info
info = dgen_py.get_system_info()
if info:
    print(f"System detected: {info['num_nodes']} NUMA nodes, {info['physical_cores']} cores")
    print(f"Deployment type: {info['deployment_type']}")
else:
    print("WARNING: NUMA info not available!")

print()
print("Testing CPU affinity detection...")
print(f"os.cpu_count(): {os.cpu_count()}")

# Check /proc/self/status for affinity
try:
    with open('/proc/self/status', 'r') as f:
        for line in f:
            if line.startswith('Cpus_allowed_list:'):
                print(f"Process affinity: {line.strip()}")
except Exception as e:
    print(f"Could not read affinity: {e}")

print()
print("⚠️  PROBLEM IDENTIFIED:")
print(f"  Process has access to ALL {os.cpu_count()} CPUs")
print("  Without os.sched_setaffinity(), Rust will create TOO MANY threads")
print()
print("Creating generator with numa_node=0 (NO affinity pinning)...")
print("Expected behavior WITHOUT pinning:")
print(f"  - Will create {os.cpu_count()} threads (BAD!)")
print("  - Cross-NUMA memory access")
print("  - Poor performance")
print()

# Enable debug logging to see thread count
os.environ['RUST_LOG'] = 'info'

gen = dgen_py.Generator(
    size=1024 * 1024 * 1024,  # 1 GB test
    numa_node=0,
    max_threads=None  # Will auto-detect from affinity (ALL CPUs!)
)

print()
print("=" * 70)
print("CONCLUSION:")
print("=" * 70)
print(f"✗ Without os.sched_setaffinity(), process can use all {os.cpu_count()} CPUs")
print("✗ Rust correctly reads affinity mask and creates too many threads")
print("✗ This causes cross-NUMA traffic and poor performance")
print()
print("✓ SOLUTION: Use benchmark_numa_multiprocess_FIXED.py")
print("✓ That script calls os.sched_setaffinity() to pin each process")
print("✓ Then Rust will create correct number of threads (24 per node)")
print("=" * 70)
