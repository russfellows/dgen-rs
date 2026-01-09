#!/usr/bin/env python3
"""
High-Performance Storage Benchmark
===================================

Producer-Consumer pipeline for testing storage write performance with dgen-py.

This benchmark:
- Uses dgen-py to generate data at >5 GB/s (CPU-bound)
- Writes data to storage using O_DIRECT (bypass page cache)
- Uses aligned buffers for optimal NVMe/SSD performance
- Implements double-buffering to keep storage always busy
- Reports detailed throughput and latency statistics

Usage:
    # Test file write (default)
    python storage_benchmark.py --size 10GB --output test.bin
    
    # Test with custom buffer pool
    python storage_benchmark.py --size 100GB --buffer-size 8MB --buffer-count 128
    
    # Test with compression/dedup
    python storage_benchmark.py --size 50GB --compress-ratio 3.0 --dedup-ratio 2.0
    
    # Skip O_DIRECT (for testing on unsupported filesystems)
    python storage_benchmark.py --size 1GB --no-direct

Requirements:
    - dgen-py (pip install dgen-py or maturin develop --release)
    - Linux for O_DIRECT support (falls back gracefully on other platforms)
"""

import os
import sys
import time
import threading
import queue
import argparse
from dataclasses import dataclass
from typing import Optional
import platform

try:
    import dgen_py
except ImportError:
    print("ERROR: dgen-py not installed")
    print("Run: pip install dgen-py")
    sys.exit(1)


# ===========================================================================
# Configuration & Stats
# ===========================================================================

@dataclass
class BenchmarkConfig:
    """Benchmark configuration"""
    total_size: int
    buffer_size: int
    buffer_count: int
    output_path: str
    dedup_ratio: float
    compress_ratio: float
    use_direct_io: bool
    numa_mode: str
    max_threads: Optional[int]


@dataclass
class BenchmarkStats:
    """Performance statistics"""
    start_time: float
    end_time: float
    bytes_generated: int
    bytes_written: int
    producer_wait_time: float = 0.0
    consumer_wait_time: float = 0.0
    write_count: int = 0
    used_direct_io: bool = False
    
    @property
    def total_time(self) -> float:
        return self.end_time - self.start_time
    
    @property
    def write_throughput_gbps(self) -> float:
        """Write throughput in GB/s"""
        if self.total_time <= 0:
            return 0.0
        return (self.bytes_written / self.total_time) / 1e9
    
    @property
    def generation_throughput_gbps(self) -> float:
        """Generation throughput in GB/s"""
        if self.total_time <= 0:
            return 0.0
        return (self.bytes_generated / self.total_time) / 1e9
    
    @property
    def avg_write_latency_ms(self) -> float:
        """Average write latency in milliseconds"""
        if self.write_count == 0:
            return 0.0
        return (self.total_time * 1000) / self.write_count
    
    @property
    def producer_utilization(self) -> float:
        """Producer utilization percentage (lower = more waiting)"""
        if self.total_time <= 0:
            return 0.0
        return (1.0 - self.producer_wait_time / self.total_time) * 100
    
    @property
    def consumer_utilization(self) -> float:
        """Consumer utilization percentage (lower = more waiting)"""
        if self.total_time <= 0:
            return 0.0
        return (1.0 - self.consumer_wait_time / self.total_time) * 100


# ===========================================================================
# Buffer Pool Management
# ===========================================================================

def create_aligned_buffer_pool(buffer_size: int, buffer_count: int):
    """
    Create a pool of page-aligned buffers using mmap.
    
    Page alignment is required for O_DIRECT on Linux.
    Using mmap ensures proper alignment without needing ctypes/cffi.
    
    Args:
        buffer_size: Size of each buffer in bytes
        buffer_count: Number of buffers to allocate
        
    Returns:
        list of mmap objects (writable, page-aligned buffers)
    """
    import mmap
    
    pool = []
    for _ in range(buffer_count):
        # Create anonymous memory-mapped region (RAM-backed, page-aligned)
        try:
            buf = mmap.mmap(-1, buffer_size, flags=mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)
            pool.append(buf)
        except Exception as e:
            print(f"Warning: Failed to create buffer via mmap: {e}")
            # Fallback to bytearray (not guaranteed to be aligned)
            pool.append(bytearray(buffer_size))
    
    return pool


# ===========================================================================
# Producer Thread (Data Generation)
# ===========================================================================

def producer_thread(
    config: BenchmarkConfig,
    empty_buffers: queue.Queue,
    full_buffers: queue.Queue,
    stats: BenchmarkStats,
    error_event: threading.Event
):
    """
    Generate data using dgen-py and fill buffers.
    
    This thread runs the Generator in streaming mode, filling buffers from
    the pool as they become available.
    """
    try:
        print(f"[Producer] Starting data generation ({config.total_size / 1e9:.2f} GB)")
        
        # Create streaming generator
        gen = dgen_py.Generator(
            size=config.total_size,
            dedup_ratio=config.dedup_ratio,
            compress_ratio=config.compress_ratio,
            numa_mode=config.numa_mode,
            max_threads=config.max_threads
        )
        
        total_generated = 0
        buffer_num = 0
        
        while not gen.is_complete() and not error_event.is_set():
            # Get an empty buffer (blocks if none available)
            wait_start = time.perf_counter()
            buf = empty_buffers.get()
            wait_time = time.perf_counter() - wait_start
            stats.producer_wait_time += wait_time
            
            if buf is None:  # Shutdown signal
                break
            
            # Generate data directly into buffer (ZERO-COPY!)
            nbytes = gen.fill_chunk(buf)
            
            if nbytes == 0:
                # Generator exhausted, return buffer and stop
                empty_buffers.put(buf)
                break
            
            total_generated += nbytes
            buffer_num += 1
            
            # Pass filled buffer to consumer
            full_buffers.put((buf, nbytes))
            
            # Progress update every 100 buffers
            if buffer_num % 100 == 0:
                progress_pct = (total_generated / config.total_size) * 100
                elapsed = time.perf_counter() - stats.start_time
                throughput = (total_generated / elapsed) / 1e9
                print(f"[Producer] Generated: {total_generated / 1e9:.2f} GB "
                      f"({progress_pct:.1f}%) @ {throughput:.2f} GB/s", end='\r')
        
        stats.bytes_generated = total_generated
        print(f"\n[Producer] Complete: {total_generated / 1e9:.2f} GB generated")
        
        # Signal consumer that we're done
        full_buffers.put(None)
        
    except Exception as e:
        print(f"\n[Producer] ERROR: {e}")
        error_event.set()
        full_buffers.put(None)


# ===========================================================================
# Consumer Thread (Disk Writer)
# ===========================================================================

def consumer_thread(
    config: BenchmarkConfig,
    empty_buffers: queue.Queue,
    full_buffers: queue.Queue,
    stats: BenchmarkStats,
    error_event: threading.Event
):
    """
    Write buffers to storage using O_DIRECT (when supported).
    
    This thread consumes filled buffers and writes them to disk, then
    returns buffers to the pool for reuse.
    """
    fd = None
    use_direct_io = False  # Track actual I/O mode used
    try:
        print(f"[Consumer] Opening file: {config.output_path}")
        
        # Open file with O_DIRECT if requested and supported
        flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
        use_direct = False
        
        if config.use_direct_io and hasattr(os, 'O_DIRECT'):
            # Try O_DIRECT first
            try:
                fd = os.open(config.output_path, flags | os.O_DIRECT, 0o644)
                use_direct = True
                use_direct_io = True
                print(f"[Consumer] ✓ O_DIRECT ENABLED (page cache bypass)")
            except OSError as e:
                # O_DIRECT not supported by filesystem, fall back to buffered
                print(f"[Consumer] ⚠ O_DIRECT FAILED ({e})")
                print(f"[Consumer] → Falling back to BUFFERED I/O (page cache will be used)")
                print(f"[Consumer] → This is common with /tmp (tmpfs) and some network filesystems")
                fd = os.open(config.output_path, flags, 0o644)
                use_direct_io = False
        else:
            if config.use_direct_io:
                print(f"[Consumer] ⚠ O_DIRECT not available on {platform.system()}")
                print(f"[Consumer] → Using BUFFERED I/O (page cache will be used)")
            fd = os.open(config.output_path, flags, 0o644)
            use_direct_io = False
        
        total_written = 0
        write_count = 0
        first_write = True
        
        while not error_event.is_set():
            # Get a filled buffer (blocks if none available)
            wait_start = time.perf_counter()
            item = full_buffers.get()
            wait_time = time.perf_counter() - wait_start
            stats.consumer_wait_time += wait_time
            
            if item is None:  # Shutdown signal
                break
            
            buf, nbytes = item
            
            # Write to file
            write_start = time.perf_counter()
            try:
                written = os.write(fd, buf[:nbytes])
            except OSError as e:
                # Handle O_DIRECT failure on first write (alignment issues)
                if first_write and use_direct:
                    print(f"\n[Consumer] ⚠ O_DIRECT write failed ({e})")
                    print(f"[Consumer] → Reopening file with BUFFERED I/O")
                    os.close(fd)
                    fd = os.open(config.output_path, flags, 0o644)
                    use_direct = False
                    use_direct_io = False
                    written = os.write(fd, buf[:nbytes])
                else:
                    raise
            
            first_write = False
            write_time = time.perf_counter() - write_start
            
            if written != nbytes:
                raise IOError(f"Partial write: {written} != {nbytes}")
            
            total_written += written
            write_count += 1
            
            # Return buffer to pool
            empty_buffers.put(buf)
            
            # Progress update every 50 writes
            if write_count % 50 == 0:
                elapsed = time.perf_counter() - stats.start_time
                throughput = (total_written / elapsed) / 1e9
                latency = (write_time * 1000)
                print(f"[Consumer] Written: {total_written / 1e9:.2f} GB "
                      f"@ {throughput:.2f} GB/s (lat: {latency:.2f} ms)", end='\r')
        
        stats.bytes_written = total_written
        stats.write_count = write_count
        # Store O_DIRECT status in stats (using a custom attribute)
        stats.used_direct_io = use_direct_io
        print(f"\n[Consumer] Complete: {total_written / 1e9:.2f} GB written ({write_count} writes)")
        
    except Exception as e:
        print(f"\n[Consumer] ERROR: {e}")
        error_event.set()
        
    finally:
        if fd is not None:
            try:
                os.fsync(fd)  # Ensure data is on disk
                os.close(fd)
            except:
                pass


# ===========================================================================
# Main Benchmark Runner
# ===========================================================================

def run_benchmark(config: BenchmarkConfig) -> BenchmarkStats:
    """
    Run the storage benchmark with producer-consumer pipeline.
    
    Returns:
        BenchmarkStats with performance metrics
    """
    print("=" * 70)
    print("HIGH-PERFORMANCE STORAGE BENCHMARK")
    print("=" * 70)
    
    # Print configuration
    print(f"\nConfiguration:")
    print(f"  Total size:      {config.total_size / 1e9:.2f} GB")
    print(f"  Buffer size:     {config.buffer_size / 1e6:.2f} MB")
    print(f"  Buffer count:    {config.buffer_count}")
    print(f"  Total pool:      {(config.buffer_size * config.buffer_count) / 1e9:.2f} GB")
    print(f"  Output file:     {config.output_path}")
    print(f"  Dedup ratio:     {config.dedup_ratio}:1")
    print(f"  Compress ratio:  {config.compress_ratio}:1")
    print(f"  Direct I/O:      {config.use_direct_io}")
    print(f"  NUMA mode:       {config.numa_mode}")
    
    # System info
    info = dgen_py.get_system_info()
    if info:
        print(f"\nSystem:")
        print(f"  NUMA nodes:      {info['num_nodes']}")
        print(f"  CPUs:            {info['logical_cpus']}")
    
    print("\n" + "=" * 70)
    
    # Create buffer pool
    print(f"\nAllocating {config.buffer_count} aligned buffers...")
    pool = create_aligned_buffer_pool(config.buffer_size, config.buffer_count)
    
    # Initialize queues
    empty_buffers = queue.Queue()
    full_buffers = queue.Queue()
    
    # Fill empty queue with all buffers
    for buf in pool:
        empty_buffers.put(buf)
    
    # Initialize stats
    stats = BenchmarkStats(
        start_time=time.perf_counter(),
        end_time=0.0,
        bytes_generated=0,
        bytes_written=0
    )
    
    # Error coordination between threads
    error_event = threading.Event()
    
    # Start threads
    print("Starting producer and consumer threads...\n")
    
    producer = threading.Thread(
        target=producer_thread,
        args=(config, empty_buffers, full_buffers, stats, error_event),
        name="Producer"
    )
    
    consumer = threading.Thread(
        target=consumer_thread,
        args=(config, empty_buffers, full_buffers, stats, error_event),
        name="Consumer"
    )
    
    producer.start()
    consumer.start()
    
    # Wait for completion
    producer.join()
    consumer.join()
    
    stats.end_time = time.perf_counter()
    
    # Print results
    print("\n" + "=" * 70)
    print("RESULTS")
    print("=" * 70)
    
    # Prominently display I/O mode
    io_mode = "O_DIRECT" if stats.used_direct_io else "BUFFERED I/O"
    io_symbol = "✓" if stats.used_direct_io else "⚠"
    print(f"\nI/O Mode: {io_symbol} {io_mode}")
    if stats.used_direct_io:
        print(f"  → Page cache bypassed (true storage performance)")
    else:
        print(f"  → Page cache used (results may not reflect true storage speed)")
    
    print(f"\nData Transfer:")
    print(f"  Generated:       {stats.bytes_generated / 1e9:.2f} GB")
    print(f"  Written:         {stats.bytes_written / 1e9:.2f} GB")
    print(f"  Total time:      {stats.total_time:.2f} s")
    
    print(f"\nThroughput:")
    print(f"  Write:           {stats.write_throughput_gbps:.2f} GB/s")
    print(f"  Generation:      {stats.generation_throughput_gbps:.2f} GB/s")
    
    print(f"\nLatency:")
    print(f"  Avg write:       {stats.avg_write_latency_ms:.2f} ms")
    print(f"  Write count:     {stats.write_count}")
    
    print(f"\nUtilization:")
    print(f"  Producer:        {stats.producer_utilization:.1f}% (waiting: {stats.producer_wait_time:.2f}s)")
    print(f"  Consumer:        {stats.consumer_utilization:.1f}% (waiting: {stats.consumer_wait_time:.2f}s)")
    
    # Bottleneck analysis
    print(f"\nBottleneck Analysis:")
    if stats.producer_utilization < 80:
        print(f"  ⚠ Producer is waiting ({stats.producer_utilization:.1f}% util)")
        print(f"    → Consumer is slower than generator")
        print(f"    → Storage is the bottleneck")
    elif stats.consumer_utilization < 80:
        print(f"  ⚠ Consumer is waiting ({stats.consumer_utilization:.1f}% util)")
        print(f"    → Generator is slower than storage")
        print(f"    → CPU/generation is the bottleneck (unlikely with dgen-py!)")
    else:
        print(f"  ✓ Balanced pipeline (both >80% utilization)")
    
    if stats.write_throughput_gbps < 3.0 and config.use_direct_io:
        print(f"  ⚠ Write throughput is lower than expected for NVMe")
        print(f"    → Check storage device capabilities")
        print(f"    → Try increasing buffer size (--buffer-size)")
    
    print("\n" + "=" * 70)
    
    return stats


# ===========================================================================
# Command Line Interface
# ===========================================================================

def parse_size(size_str: str) -> int:
    """Parse size string like '10GB', '500MB', '1TB'"""
    size_str = size_str.upper().strip()
    
    multipliers = {
        'KB': 1024,
        'MB': 1024**2,
        'GB': 1024**3,
        'TB': 1024**4,
    }
    
    for suffix, multiplier in multipliers.items():
        if size_str.endswith(suffix):
            num = float(size_str[:-len(suffix)])
            return int(num * multiplier)
    
    # Try as raw number
    try:
        return int(size_str)
    except ValueError:
        raise ValueError(f"Invalid size: {size_str}. Use format like '10GB', '500MB', etc.")


def main():
    parser = argparse.ArgumentParser(
        description="High-performance storage benchmark using dgen-py",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Write 10 GB to test file
  %(prog)s --size 10GB --output /tmp/test.bin
  
  # Large test with custom buffers
  %(prog)s --size 100GB --buffer-size 8MB --buffer-count 256
  
  # Test with compression and deduplication
  %(prog)s --size 50GB --compress-ratio 3.0 --dedup-ratio 2.0
  
  # Quick test without O_DIRECT
  %(prog)s --size 1GB --no-direct --output /tmp/test.bin
"""
    )
    
    parser.add_argument(
        '--size', '-s',
        type=parse_size,
        default='10GB',
        help='Total data size to write (e.g., 10GB, 500MB, 1TB). Default: 10GB'
    )
    
    parser.add_argument(
        '--output', '-o',
        type=str,
        default='benchmark_output.bin',
        help='Output file path. Default: benchmark_output.bin'
    )
    
    parser.add_argument(
        '--buffer-size',
        type=parse_size,
        default='4MB',
        help='Size of each buffer (e.g., 4MB, 8MB). Default: 4MB'
    )
    
    parser.add_argument(
        '--buffer-count',
        type=int,
        default=250,
        help='Number of buffers in pool. Default: 250 (1GB pool with 4MB buffers)'
    )
    
    parser.add_argument(
        '--dedup-ratio',
        type=float,
        default=1.0,
        help='Deduplication ratio (1.0 = no dedup). Default: 1.0'
    )
    
    parser.add_argument(
        '--compress-ratio',
        type=float,
        default=1.0,
        help='Compression ratio (1.0 = incompressible). Default: 1.0'
    )
    
    parser.add_argument(
        '--no-direct',
        action='store_true',
        help='Disable O_DIRECT (use buffered I/O)'
    )
    
    parser.add_argument(
        '--numa-mode',
        choices=['auto', 'force', 'disabled'],
        default='auto',
        help='NUMA optimization mode. Default: auto'
    )
    
    parser.add_argument(
        '--max-threads',
        type=int,
        default=None,
        help='Maximum threads for data generation. Default: auto-detect'
    )
    
    args = parser.parse_args()
    
    # Validate
    if args.buffer_size % 4096 != 0:
        print(f"Warning: Buffer size should be multiple of 4096 for optimal O_DIRECT performance")
    
    # Create config
    config = BenchmarkConfig(
        total_size=args.size,
        buffer_size=args.buffer_size,
        buffer_count=args.buffer_count,
        output_path=args.output,
        dedup_ratio=args.dedup_ratio,
        compress_ratio=args.compress_ratio,
        use_direct_io=not args.no_direct,
        numa_mode=args.numa_mode,
        max_threads=args.max_threads
    )
    
    # Run benchmark
    try:
        stats = run_benchmark(config)
        
        # Exit with success
        return 0
        
    except KeyboardInterrupt:
        print("\n\nBenchmark interrupted by user")
        return 130
        
    except Exception as e:
        print(f"\n\nBenchmark failed: {e}")
        import traceback
        traceback.print_exc()
        return 1


if __name__ == "__main__":
    sys.exit(main())
