#!/usr/bin/env python3
"""
PROPER NUMA Multi-Process Benchmark

CRITICAL REQUIREMENTS:
1. Create N independent Python processes (one per NUMA node)
2. Each process allocates LOCAL buffer on its LOCAL NUMA node
3. Each process uses ONLY its LOCAL CPU cores
4. Aggregate results across all processes

This achieves TRUE data locality and avoids cross-node memory traffic.
"""

import dgen_py
import time
import multiprocessing as mp
from typing import Optional


def worker_process(
    numa_node: int,
    size_per_node: int,
    result_queue: mp.Queue,
    barrier: mp.Barrier
):
    """
    Worker process for one NUMA node.
    
    CRITICAL: This process will:
    1. Allocate buffer on LOCAL NUMA node (numa_node parameter)
    2. Use ONLY cores from LOCAL NUMA node (automatic via numa_node binding)
    3. Generate data entirely within LOCAL memory (zero cross-node traffic)
    """
    try:
        # Get system info to verify NUMA binding
        info = dgen_py.get_system_info()
        
        print(f"[Node {numa_node}] Starting worker process (PID: {mp.current_process().pid})")
        if info:
            print(f"[Node {numa_node}] System: {info['num_nodes']} nodes, {info['physical_cores']} total cores")
        
        # Create generator bound to THIS NUMA node
        # CRITICAL: numa_node parameter ensures:
        #   - Memory allocated on node {numa_node}
        #   - Threads use only cores from node {numa_node}
        gen = dgen_py.Generator(
            size=size_per_node,
            dedup_ratio=1.0,
            compress_ratio=1.0,
            numa_mode="force",  # FORCE NUMA optimization
            max_threads=None,   # Use all cores on THIS node
            numa_node=numa_node,  # CRITICAL: Bind to specific NUMA node
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
            'success': True
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


def run_numa_benchmark(total_size: int, num_nodes: Optional[int] = None):
    """
    Run multi-process NUMA benchmark.
    
    Args:
        total_size: Total bytes to generate across ALL nodes
        num_nodes: Number of NUMA nodes (None = auto-detect)
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
    print("PROPER MULTI-PROCESS NUMA BENCHMARK")
    print("=" * 70)
    print(f"System: {detected_nodes} NUMA nodes, {physical_cores} physical cores")
    print(f"Deployment: {info['deployment_type']}")
    print(f"Total size: {total_size / (1024**3):.0f} GB")
    print(f"Processes: {num_nodes} (one per NUMA node)")
    print(f"Size per node: {(total_size / num_nodes) / (1024**3):.1f} GB")
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
            args=(node_id, size_per_node, result_queue, barrier),
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
    
    for result in results:
        if result.get('success'):
            node = result['numa_node']
            duration = result['duration']
            throughput = result['throughput']
            bytes_gen = result['bytes_generated']
            
            print(f"Node {node}: {duration:.3f}s | {throughput:.2f} GB/s | {bytes_gen:,} bytes")
            
            total_bytes += bytes_gen
            max_duration = max(max_duration, duration)
            successful_nodes += 1
        else:
            print(f"Node {result['numa_node']}: FAILED - {result.get('error', 'Unknown error')}")
    
    # Aggregate results
    print()
    print("=" * 70)
    print("AGGREGATE RESULTS")
    print("=" * 70)
    
    if successful_nodes > 0:
        aggregate_throughput = (total_bytes / (1024**3)) / max_duration
        per_core_throughput = aggregate_throughput / physical_cores
        
        print(f"Successful nodes: {successful_nodes}/{num_nodes}")
        print(f"Total bytes: {total_bytes:,} ({total_bytes / (1024**3):.1f} GB)")
        print(f"Max duration: {max_duration:.3f}s (slowest node)")
        print(f"AGGREGATE THROUGHPUT: {aggregate_throughput:.2f} GB/s")
        print(f"Per-core throughput: {per_core_throughput:.2f} GB/s")
        print()
        
        if detected_nodes > 1:
            print(f"NUMA Scaling:")
            print(f"  Expected on 384 cores: {per_core_throughput * 384:.0f} GB/s")
            print(f"  Target (80 GB/s storage): {aggregate_throughput / 80:.1f}x faster")
    else:
        print("ERROR: All nodes failed!")
    
    print("=" * 70)


def main():
    # Test with 100 GB total (same as single-process benchmark)
    TEST_SIZE = 100 * 1024 * 1024 * 1024  # 100 GB
    
    print("Starting multi-process NUMA benchmark...")
    print("This will create one Python process per NUMA node.")
    print("Each process will:")
    print("  1. Allocate buffer on its LOCAL NUMA node")
    print("  2. Use only its LOCAL CPU cores")
    print("  3. Generate data entirely within LOCAL memory")
    print()
    
    run_numa_benchmark(TEST_SIZE)


if __name__ == "__main__":
    # CRITICAL: Use 'spawn' to create truly independent processes
    mp.set_start_method('spawn', force=True)
    main()
