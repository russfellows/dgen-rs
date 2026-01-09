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
    num_writers: int = 1


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
    writer_stats: dict = None  # Per-writer statistics
    
    def __post_init__(self):
        if self.writer_stats is None:
            self.writer_stats = {}
    
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
        """Consumer utilization percentage (lower = more waiting)
        
        For multiple writers, this uses the average wait time across all writers
        to avoid inflated totals from summing independent thread wait times.
        """
        if self.total_time <= 0:
            return 0.0
        
        # If we have per-writer stats, calculate average wait time
        if self.writer_stats:
            num_writers = len(self.writer_stats)
            if num_writers > 0:
                avg_wait_time = self.consumer_wait_time / num_writers
                return (1.0 - avg_wait_time / self.total_time) * 100
        
        # Single writer or no stats
        return (1.0 - self.consumer_wait_time / self.total_time) * 100


# ===========================================================================
# Auto-Tuning
# ===========================================================================

def auto_tune_settings(buffer_size: int = None, buffer_count: int = None, 
                       num_writers: int = None) -> tuple:
    """
    Automatically tune buffer and writer settings based on system capabilities.
    
    Scaling guidelines based on empirical testing:
    - Small systems (8 CPUs): 4 writers, 128 buffers
    - Medium systems (16-32 CPUs): 8 writers, 256 buffers
    - Large systems (64-128 CPUs): 16-32 writers, 512+ buffers
    
    Returns:
        tuple: (buffer_size, buffer_count, num_writers)
    """
    import multiprocessing
    
    cpu_count = multiprocessing.cpu_count()
    
    # Auto-tune buffer size (default 8MB for NVMe, 4MB for smaller systems)
    if buffer_size is None:
        if cpu_count <= 8:
            buffer_size = 4 * 1024 * 1024  # 4 MB
        else:
            buffer_size = 8 * 1024 * 1024  # 8 MB
    
    # Auto-tune number of writers based on CPU count
    if num_writers is None:
        if cpu_count <= 8:
            num_writers = 4
        elif cpu_count <= 16:
            num_writers = 8
        elif cpu_count <= 32:
            num_writers = 12
        elif cpu_count <= 64:
            num_writers = 16
        else:
            # For very large systems (128+ CPUs)
            num_writers = min(32, cpu_count // 4)
    
    # Auto-tune buffer count based on writers and system size
    if buffer_count is None:
        # Rule: buffer_count >= num_writers * 16 (ensure writers don't starve)
        # Also scale with CPU count for better buffering
        min_buffers = num_writers * 16
        
        if cpu_count <= 8:
            buffer_count = max(128, min_buffers)
        elif cpu_count <= 16:
            buffer_count = max(256, min_buffers)
        elif cpu_count <= 64:
            buffer_count = max(512, min_buffers)
        else:
            buffer_count = max(1024, min_buffers)
    
    return (buffer_size, buffer_count, num_writers)


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
    full_buffers_list: list,  # List of queues for multiple writers
    stats: BenchmarkStats,
    error_event: threading.Event
):
    """
    Generate data using dgen-py and fill buffers.
    
    This thread runs the Generator in streaming mode, filling buffers from
    the pool and distributing them round-robin to multiple writer threads.
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
        writer_index = 0  # Round-robin index for distributing to writers
        
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
            
            # Pass filled buffer to consumer (round-robin across writers)
            full_buffers_list[writer_index].put((buf, nbytes))
            writer_index = (writer_index + 1) % config.num_writers
            
            # Progress update every 100 buffers
            if buffer_num % 100 == 0:
                progress_pct = (total_generated / config.total_size) * 100
                elapsed = time.perf_counter() - stats.start_time
                throughput = (total_generated / elapsed) / 1e9
                print(f"[Producer] Generated: {total_generated / 1e9:.2f} GB "
                      f"({progress_pct:.1f}%) @ {throughput:.2f} GB/s", end='\r')
        
        stats.bytes_generated = total_generated
        print(f"\n[Producer] Complete: {total_generated / 1e9:.2f} GB generated")
        
        # Signal all consumers that we're done
        for full_buffers in full_buffers_list:
            full_buffers.put(None)
        
    except Exception as e:
        print(f"\n[Producer] ERROR: {e}")
        error_event.set()
        # Signal all consumers to stop
        for full_buffers in full_buffers_list:
            full_buffers.put(None)


# ===========================================================================
# Consumer Thread (Disk Writer)
# ===========================================================================

def consumer_thread(
    writer_id: int,
    config: BenchmarkConfig,
    empty_buffers: queue.Queue,
    full_buffers: queue.Queue,
    stats: BenchmarkStats,
    stats_lock: threading.Lock,
    error_event: threading.Event
):
    """
    Write buffers to storage using O_DIRECT (when supported).
    
    Multiple instances of this thread can run in parallel, each writing
    sequentially to keep track of their position in the file.
    """
    fd = None
    use_direct_io = False  # Track actual I/O mode used
    thread_name = f"Writer-{writer_id}"
    
    # Per-writer statistics
    local_bytes_written = 0
    local_write_count = 0
    local_wait_time = 0.0
    
    try:
        if writer_id == 0:  # Only first writer prints
            print(f"[{thread_name}] Opening file: {config.output_path}")
        
        # Open file with O_DIRECT if requested and supported
        flags = os.O_WRONLY | os.O_CREAT
        if writer_id == 0:
            flags |= os.O_TRUNC  # Only first writer truncates
        
        use_direct = False
        
        if config.use_direct_io and hasattr(os, 'O_DIRECT'):
            # Try O_DIRECT first
            try:
                fd = os.open(config.output_path, flags | os.O_DIRECT, 0o644)
                use_direct = True
                use_direct_io = True
                if writer_id == 0:
                    print(f"[{thread_name}] ✓ O_DIRECT ENABLED (page cache bypass)")
                    if config.num_writers > 1:
                        print(f"[Writers] Launching {config.num_writers} parallel writer threads")
            except OSError as e:
                # O_DIRECT not supported by filesystem, fall back to buffered
                if writer_id == 0:
                    print(f"[{thread_name}] ⚠ O_DIRECT FAILED ({e})")
                    print(f"[{thread_name}] → Falling back to BUFFERED I/O (page cache will be used)")
                    print(f"[{thread_name}] → This is common with /tmp (tmpfs) and some network filesystems")
                fd = os.open(config.output_path, flags, 0o644)
                use_direct_io = False
        else:
            if config.use_direct_io and writer_id == 0:
                print(f"[{thread_name}] ⚠ O_DIRECT not available on {platform.system()}")
                print(f"[{thread_name}] → Using BUFFERED I/O (page cache will be used)")
            fd = os.open(config.output_path, flags, 0o644)
            use_direct_io = False
        
        # Track current file offset for this writer
        current_offset = 0
        first_write = True
        
        while not error_event.is_set():
            # Get a filled buffer (blocks if none available)
            wait_start = time.perf_counter()
            item = full_buffers.get()
            wait_time = time.perf_counter() - wait_start
            local_wait_time += wait_time
            
            if item is None:  # Shutdown signal
                break
            
            buf, nbytes = item
            
            # Write to file at current offset using pwrite (thread-safe positioned write)
            write_start = time.perf_counter()
            try:
                written = os.pwrite(fd, buf[:nbytes], current_offset)
                current_offset += written
            except (OSError, AttributeError) as e:
                # Handle O_DIRECT failure or pwrite not available
                if first_write and use_direct:
                    if writer_id == 0:
                        print(f"\n[{thread_name}] ⚠ O_DIRECT write failed ({e})")
                        print(f"[{thread_name}] → Reopening file with BUFFERED I/O")
                    os.close(fd)
                    fd = os.open(config.output_path, flags, 0o644)
                    use_direct = False
                    use_direct_io = False
                    written = os.pwrite(fd, buf[:nbytes], current_offset)
                    current_offset += written
                elif isinstance(e, AttributeError):
                    # pwrite not available, fall back to sequential write
                    written = os.write(fd, buf[:nbytes])
                else:
                    raise
            
            first_write = False
            
            if written != nbytes:
                raise IOError(f"Partial write: {written} != {nbytes}")
            
            local_bytes_written += written
            local_write_count += 1
            
            # Return buffer to pool
            empty_buffers.put(buf)
            
            # Progress update every 50 writes (only writer 0)
            if writer_id == 0 and local_write_count % 50 == 0:
                with stats_lock:
                    total_written = stats.bytes_written
                elapsed = time.perf_counter() - stats.start_time
                throughput = (total_written / elapsed) / 1e9 if elapsed > 0 else 0
                print(f"[Writers] Written: {total_written / 1e9:.2f} GB "
                      f"@ {throughput:.2f} GB/s ({config.num_writers} threads)", end='\r')
        
        # Update global stats (thread-safe)
        with stats_lock:
            stats.bytes_written += local_bytes_written
            stats.write_count += local_write_count
            stats.consumer_wait_time += local_wait_time
            stats.used_direct_io = use_direct_io  # Last writer wins (should all be same)
            
            # Store per-writer stats
            stats.writer_stats[writer_id] = {
                'bytes': local_bytes_written,
                'writes': local_write_count,
                'wait_time': local_wait_time
            }
        
        if writer_id == 0:
            print(f"\n[{thread_name}] Complete: {local_bytes_written / 1e9:.2f} GB written ({local_write_count} writes)")
        
    except Exception as e:
        print(f"\n[{thread_name}] ERROR: {e}")
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
    print(f"  Writer threads:  {config.num_writers}")
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
    full_buffers_list = [queue.Queue() for _ in range(config.num_writers)]
    
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
    
    # Shared lock for stats updates
    stats_lock = threading.Lock()
    
    # Start threads
    print("Starting producer and writer threads...\n")
    
    producer = threading.Thread(
        target=producer_thread,
        args=(config, empty_buffers, full_buffers_list, stats, error_event),
        name="Producer"
    )
    
    # Create multiple writer threads
    consumers = []
    for i in range(config.num_writers):
        consumer = threading.Thread(
            target=consumer_thread,
            args=(i, config, empty_buffers, full_buffers_list[i], stats, stats_lock, error_event),
            name=f"Writer-{i}"
        )
        consumers.append(consumer)
    
    producer.start()
    for consumer in consumers:
        consumer.start()
    
    # Wait for completion
    producer.join()
    for consumer in consumers:
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
    
    # Show per-writer stats if multiple writers
    if config.num_writers > 1 and stats.writer_stats:
        print(f"\nPer-Writer Statistics:")
        for writer_id in sorted(stats.writer_stats.keys()):
            wstats = stats.writer_stats[writer_id]
            print(f"  Writer-{writer_id}: {wstats['bytes'] / 1e9:.2f} GB ({wstats['writes']} writes, "
                  f"wait: {wstats['wait_time']:.2f}s)")
    
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
        '--auto',
        action='store_true',
        help='Auto-tune buffer-size, buffer-count, and num-writers based on CPU count'
    )
    
    parser.add_argument(
        '--buffer-size',
        type=parse_size,
        default=None,
        help='Size of each buffer (e.g., 4MB, 8MB). Default: auto-tuned or 4MB'
    )
    
    parser.add_argument(
        '--buffer-count',
        type=int,
        default=None,
        help='Number of buffers in pool. Default: auto-tuned or 250'
    )
    
    parser.add_argument(
        '--num-writers',
        type=int,
        default=None,
        help='Number of parallel writer threads (1-64). Default: auto-tuned or 1'
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
    
    # Auto-tune settings if requested or if any setting is None
    if args.auto or args.buffer_size is None or args.buffer_count is None or args.num_writers is None:
        tuned_buffer_size, tuned_buffer_count, tuned_num_writers = auto_tune_settings(
            buffer_size=args.buffer_size,
            buffer_count=args.buffer_count,
            num_writers=args.num_writers
        )
        
        # Apply auto-tuned values for None settings
        if args.buffer_size is None:
            args.buffer_size = tuned_buffer_size
        if args.buffer_count is None:
            args.buffer_count = tuned_buffer_count
        if args.num_writers is None:
            args.num_writers = tuned_num_writers
        
        if args.auto:
            import multiprocessing
            print(f"Auto-tuning for {multiprocessing.cpu_count()} CPU system:")
            print(f"  Buffer size:   {args.buffer_size / 1e6:.0f} MB")
            print(f"  Buffer count:  {args.buffer_count}")
            print(f"  Writer threads: {args.num_writers}")
            print()
    
    # Validate
    if args.buffer_size % 4096 != 0:
        print(f"Warning: Buffer size should be multiple of 4096 for optimal O_DIRECT performance")
    
    if args.num_writers < 1 or args.num_writers > 64:
        print(f"Error: --num-writers must be between 1 and 64")
        return 1
    
    if args.num_writers > args.buffer_count:
        print(f"Warning: --num-writers ({args.num_writers}) > --buffer-count ({args.buffer_count})")
        print(f"         Writers may starve waiting for buffers. Consider increasing buffer count.")
    
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
        max_threads=args.max_threads,
        num_writers=args.num_writers
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
