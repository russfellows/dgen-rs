#!/usr/bin/env python3
"""
PROPER NUMA Multi-Process Benchmark - FIXED VERSION

CRITICAL REQUIREMENTS:
1. Create N independent Python processes (one per NUMA node)
2. PIN each process to its NUMA node's cores using os.sched_setaffinity()
3. Each process allocates LOCAL buffer on its LOCAL NUMA node
4. Each process uses ONLY its LOCAL CPU cores
5. Aggregate results across all processes

This achieves TRUE data locality and avoids cross-node memory traffic.
"""

import dgen_py
import time
import multiprocessing as mp
from typing import Optional
import os


def get_numa_node_cpus(numa_node: int, physical_only: bool = False) -> list:
    """
    Get list of CPU IDs for a specific NUMA node.
    
    Args:
        numa_node: NUMA node ID (0, 1, ...)
        physical_only: If True, return only physical cores (exclude hyperthreads)
    
    Reads from /sys/devices/system/node/nodeN/cpulist
    Returns list of CPU IDs (e.g., [0,1,2,...,23] for node 0)
    """
    try:
        cpu_list_file = f"/sys/devices/system/node/node{numa_node}/cpulist"
        with open(cpu_list_file, 'r') as f:
            cpu_list = f.read().strip()
        
        # Parse CPU list (e.g., "0-23" or "0-11,24-35")
        cpus = []
        for range_str in cpu_list.split(','):
            if '-' in range_str:
                start, end = range_str.split('-')
                cpus.extend(range(int(start), int(end) + 1))
            else:
                cpus.append(int(range_str))
        
        # Filter to physical cores only if requested
        if physical_only:
            # On Linux with hyperthreading, logical CPU layout is:
            # Physical cores: 0-(N-1), Hyperthreads: N-(2N-1)
            # For NUMA systems, each node gets half the physical cores
            # We need to filter out the hyperthread siblings
            physical_cpus = []
            for cpu in cpus:
                # Read thread_siblings_list to identify physical vs hyperthread
                try:
                    siblings_file = f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"
                    with open(siblings_file, 'r') as f:
                        siblings = f.read().strip()
                    
                    # Parse siblings (e.g., "0,48" means CPU 0 and CPU 48 are hyperthreads)
                    sibling_cpus = []
                    for s in siblings.split(','):
                        if '-' in s:
                            start, end = s.split('-')
                            sibling_cpus.extend(range(int(start), int(end) + 1))
                        else:
                            sibling_cpus.append(int(s))
                    
                    # Keep the CPU if it's the lowest-numbered sibling (physical core)
                    if cpu == min(sibling_cpus):
                        physical_cpus.append(cpu)
                except:
                    # If we can't read siblings, assume it's a physical core
                    physical_cpus.append(cpu)
            
            return sorted(physical_cpus)
        
        return cpus
    except Exception as e:
        print(f"[Node {numa_node}] WARNING: Could not read NUMA CPU list: {e}")
        print(f"[Node {numa_node}] Falling back to /proc/cpuinfo parsing")
        
        # Fallback: parse /proc/cpuinfo
        cpus = []
        try:
            with open('/proc/cpuinfo', 'r') as f:
                current_cpu = None
                for line in f:
                    if line.startswith('processor'):
                        current_cpu = int(line.split(':')[1].strip())
                    elif line.startswith('physical id'):
                        # This is socket ID (on multi-socket systems, socket == NUMA node)
                        socket = int(line.split(':')[1].strip())
                        if socket == numa_node and current_cpu is not None:
                            cpus.append(current_cpu)
        except Exception as e2:
            print(f"[Node {numa_node}] ERROR: Could not determine NUMA CPUs: {e2}")
            return []
        
        return sorted(cpus)


def worker_process(
    numa_node: int,
    size_per_node: int,
    result_queue: mp.Queue,
    barrier: mp.Barrier,
    physical_cores_only: bool = False
):
    """
    Worker process for one NUMA node.
    
    CRITICAL: This process will:
    1. PIN itself to NUMA node's CPUs using os.sched_setaffinity()
    2. Allocate buffer on LOCAL NUMA node
    3. Use ONLY cores from LOCAL NUMA node
    4. Generate data entirely within LOCAL memory (zero cross-node traffic)
    
    Args:
        numa_node: NUMA node ID
        size_per_node: Bytes to generate
        result_queue: Queue for results
        barrier: Synchronization barrier
        physical_cores_only: If True, use only physical cores (no hyperthreads)
    """
    try:
        # CRITICAL FIX: Pin this Python process to the NUMA node's CPUs
        numa_cpus = get_numa_node_cpus(numa_node, physical_only=physical_cores_only)
        
        if not numa_cpus:
            print(f"[Node {numa_node}] ERROR: Could not determine CPU list for NUMA node")
            result_queue.put({
                'numa_node': numa_node,
                'success': False,
                'error': 'Could not determine NUMA node CPUs'
            })
            return
        
        print(f"[Node {numa_node}] Starting worker process (PID: {os.getpid()})")
        if physical_cores_only:
            print(f"[Node {numa_node}] Pinning to PHYSICAL cores only: {numa_cpus}")
        else:
            print(f"[Node {numa_node}] Pinning to all logical CPUs: {numa_cpus}")
        
        # PIN THE PROCESS to this NUMA node's cores
        os.sched_setaffinity(0, numa_cpus)
        
        # Verify the pinning worked
        actual_affinity = os.sched_getaffinity(0)
        print(f"[Node {numa_node}] Process affinity confirmed: {sorted(actual_affinity)}")
        print(f"[Node {numa_node}] CPU count: {len(actual_affinity)} cores")
        
        # Get system info to verify NUMA binding
        info = dgen_py.get_system_info()
        if info:
            print(f"[Node {numa_node}] System: {info['num_nodes']} nodes, {info['physical_cores']} total cores")
        
        # Create generator bound to THIS NUMA node
        # CRITICAL: With os.sched_setaffinity() set, the Rust code will:
        #   1. Read /proc/self/status and see only this NUMA node's cores
        #   2. Create correct number of threads (24, not 48)
        #   3. All threads stay on local NUMA node cores
        gen = dgen_py.Generator(
            size=size_per_node,
            dedup_ratio=1.0,
            compress_ratio=1.0,
            numa_mode="auto",
            max_threads=None,   # Auto-detect from CPU affinity (should be 24)
            numa_node=numa_node,  # Tell Rust which NUMA node for memory allocation
            chunk_size=32 * 1024 * 1024  # 32 MB chunks
        )
        
        chunk_size = gen.chunk_size
        buffer = bytearray(chunk_size)
        
        print(f"[Node {numa_node}] Buffer allocated: {chunk_size / (1024**2):.0f} MB chunks")
        print(f"[Node {numa_node}] Waiting at barrier for synchronized start...")
        
        # Synchronize all processes to start at same time
        barrier.wait()
        
        print(f"[Node {numa_node}] Starting generation...")
        start_time = time.perf_counter()
        
        bytes_generated = 0
        while not gen.is_complete():
            nbytes = gen.fill_chunk(buffer)
            if nbytes == 0:
                break
            bytes_generated += nbytes
        
        end_time = time.perf_counter()
        duration = end_time - start_time
        throughput = (size_per_node / (1024**3)) / duration
        
        print(f"[Node {numa_node}] COMPLETE: {duration:.3f}s, {throughput:.2f} GB/s")
        
        # Send results back
        result_queue.put({
            'numa_node': numa_node,
            'duration': duration,
            'bytes_generated': bytes_generated,
            'throughput': throughput,
            'success': True,
            'cpu_count': len(actual_affinity)
        })
        
    except Exception as e:
        print(f"[Node {numa_node}] ERROR: {e}")
        import traceback
        traceback.print_exc()
        result_queue.put({
            'numa_node': numa_node,
            'success': False,
            'error': str(e)
        })


def run_numa_benchmark(total_size: int, num_nodes: Optional[int] = None, physical_cores_only: bool = False):
    """
    Run multi-process NUMA benchmark.
    
    Args:
        total_size: Total bytes to generate across ALL nodes
        num_nodes: Number of NUMA nodes (None = auto-detect)
        physical_cores_only: If True, use only physical cores (exclude hyperthreads)
    """
    # Get system info
    info = dgen_py.get_system_info()
    if not info:
        print("ERROR: NUMA info not available!")
        print("This benchmark requires NUMA support compiled in.")
        return
    
    detected_nodes = info['num_nodes']
    physical_cores = info['physical_cores']
    
    if num_nodes is None:
        num_nodes = detected_nodes
    
    print("=" * 70)
    print("PROPER MULTI-PROCESS NUMA BENCHMARK - FIXED VERSION")
    print("=" * 70)
    print(f"System: {detected_nodes} NUMA nodes, {physical_cores} physical cores")
    print(f"Deployment: {info['deployment_type']}")
    print(f"Total size: {total_size / (1024**3):.0f} GB")
    print(f"Processes: {num_nodes} (one per NUMA node)")
    print(f"Size per node: {(total_size / num_nodes) / (1024**3):.1f} GB")
    if physical_cores_only:
        print("Core mode: PHYSICAL CORES ONLY (hyperthreading disabled)")
    else:
        print("Core mode: ALL LOGICAL CPUS (hyperthreading enabled)")
    print()
    print("CRITICAL FIX: Processes will be pinned via os.sched_setaffinity()")
    print()
    
    if detected_nodes == 1:
        print("WARNING: This is a UMA system (1 NUMA node)")
        print("Multi-process won't help performance, but testing anyway...")
        print()
    
    # Calculate size per node
    size_per_node = total_size // num_nodes
    
    # Create synchronization primitives
    result_queue = mp.Queue()
    barrier = mp.Barrier(num_nodes)  # All processes wait here before starting
    
    # Spawn worker processes (one per NUMA node)
    processes = []
    print(f"Spawning {num_nodes} worker processes...")
    for node_id in range(num_nodes):
        p = mp.Process(
            target=worker_process,
            args=(node_id, size_per_node, result_queue, barrier, physical_cores_only),
            name=f"NUMA-Node-{node_id}"
        )
        p.start()
        processes.append(p)
        print(f"  ✓ Spawned process for NUMA node {node_id} (PID: {p.pid})")
    
    print()
    print("All processes spawned. Workers will synchronize and start together...")
    print("=" * 70)
    print()
    
    # Wait for all processes to complete
    for p in processes:
        p.join()
    
    # Collect results
    results = []
    while not result_queue.empty():
        results.append(result_queue.get())
    
    # Sort by NUMA node
    results.sort(key=lambda x: x.get('numa_node', 999))
    
    # Display results
    print()
    print("=" * 70)
    print("PER-NODE RESULTS")
    print("=" * 70)
    
    total_bytes = 0
    max_duration = 0
    successful_nodes = 0
    total_cpu_count = 0
    
    for result in results:
        if result.get('success'):
            node = result['numa_node']
            duration = result['duration']
            throughput = result['throughput']
            bytes_gen = result['bytes_generated']
            cpu_count = result.get('cpu_count', 0)
            
            print(f"Node {node}: {duration:.3f}s | {throughput:.2f} GB/s | {bytes_gen:,} bytes | {cpu_count} CPUs")
            
            total_bytes += bytes_gen
            max_duration = max(max_duration, duration)
            successful_nodes += 1
            total_cpu_count += cpu_count
        else:
            print(f"Node {result['numa_node']}: FAILED - {result.get('error', 'Unknown error')}")
    
    # Aggregate results
    print()
    print("=" * 70)
    print("AGGREGATE RESULTS")
    print("=" * 70)
    
    if successful_nodes > 0:
        aggregate_throughput = (total_bytes / (1024**3)) / max_duration
        
        # Calculate per-core based on PHYSICAL cores used
        physical_cores_used = total_cpu_count // 2 if not physical_cores_only else total_cpu_count
        per_core_throughput = aggregate_throughput / physical_cores_used
        per_logical_cpu = aggregate_throughput / total_cpu_count
        
        print(f"Successful nodes: {successful_nodes}/{num_nodes}")
        print(f"Total CPUs used: {total_cpu_count} logical ({physical_cores_used} physical cores)")
        print(f"Total bytes: {total_bytes:,} ({total_bytes / (1024**3):.1f} GB)")
        print(f"Max duration: {max_duration:.3f}s (slowest node)")
        print(f"AGGREGATE THROUGHPUT: {aggregate_throughput:.2f} GB/s")
        print(f"Per PHYSICAL core: {per_core_throughput:.2f} GB/s")
        print(f"Per LOGICAL CPU: {per_logical_cpu:.2f} GB/s")
        print()
        
        if detected_nodes > 1:
            print(f"NUMA Scaling:")
            print(f"  Expected on 384 physical cores: {per_core_throughput * 384:.0f} GB/s")
            print(f"  Target (80 GB/s storage): {aggregate_throughput / 80:.1f}x faster")
        
        # Performance expectations
        print()
        print("PERFORMANCE ANALYSIS:")
        print(f"  Physical core throughput: {per_core_throughput:.2f} GB/s")
        if per_core_throughput < 2.5:
            print("  ⚠️  Low per-core performance for multi-socket NUMA")
            print("  Typical causes:")
            print("    - Memory bandwidth bottleneck (shared per socket)")
            print("    - CPU frequency scaling/throttling")
            print("    - Background system activity")
        elif per_core_throughput >= 3.0 and per_core_throughput < 5.0:
            print("  ✅ Reasonable for multi-socket NUMA system")
            print("  Note: Multi-socket has lower bandwidth/core than UMA")
        elif per_core_throughput >= 5.0:
            print("  ✅ EXCELLENT performance for multi-socket NUMA!")
        
        if not physical_cores_only:
            print()
            print("  TIP: Try running with --physical-cores-only flag")
            print("  Hyperthreading can reduce performance for memory-intensive workloads")
    else:
        print("ERROR: All nodes failed!")
    
    print("=" * 70)


def main():
    import argparse
    
    parser = argparse.ArgumentParser(description='NUMA multi-process benchmark')
    parser.add_argument('--size-gb', type=int, default=1024,
                       help='Total size in GB (default: 1024)')
    parser.add_argument('--physical-cores-only', action='store_true',
                       help='Use only physical cores (disable hyperthreading)')
    args = parser.parse_args()
    
    # Test with specified size
    TEST_SIZE = args.size_gb * 1024 * 1024 * 1024
    
    print("Starting multi-process NUMA benchmark (FIXED VERSION)...")
    print("This will create one Python process per NUMA node.")
    print("Each process will:")
    print("  1. PIN itself to NUMA node cores (os.sched_setaffinity)")
    print("  2. Allocate buffer on its LOCAL NUMA node")
    print("  3. Use only its LOCAL CPU cores")
    print("  4. Generate data entirely within LOCAL memory")
    print()
    
    run_numa_benchmark(TEST_SIZE, physical_cores_only=args.physical_cores_only)


if __name__ == "__main__":
    # CRITICAL: Use 'spawn' to create truly independent processes
    mp.set_start_method('spawn', force=True)
    main()
