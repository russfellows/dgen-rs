#!/usr/bin/env python3
"""
io_uring-based Storage Benchmark

Uses io_uring to batch I/O operations and eliminate syscall overhead.
Goal: Match single-buffer performance (1.26 GB/s) with only 2GB memory footprint.

io_uring advantages:
- Batch multiple I/O operations (submit many writes at once)
- Async completion (no waiting per write)
- Reduced context switches (submit all, then wait for completions)
"""

import os
import sys
import time
import mmap
import argparse
import ctypes
from pathlib import Path

try:
    import dgen_py
except ImportError:
    print("Error: dgen-py not installed")
    print("Install with: uv pip install dgen-py")
    sys.exit(1)

try:
    import liburing as uring
except ImportError:
    print("Error: liburing not installed")
    print("Install with: uv pip install liburing")
    sys.exit(1)


def create_aligned_buffer(size: int) -> memoryview:
    """Create page-aligned buffer for O_DIRECT."""
    buf = mmap.mmap(-1, size, mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)
    return memoryview(buf)


def format_bytes(size: int) -> str:
    """Format bytes as human-readable string."""
    for unit in ['B', 'KB', 'MB', 'GB', 'TB']:
        if size < 1024.0:
            return f"{size:.2f} {unit}"
        size /= 1024.0
    return f"{size:.2f} PB"


def format_duration(seconds: float) -> str:
    """Format duration as human-readable string."""
    if seconds < 1.0:
        return f"{seconds*1000:.2f} ms"
    elif seconds < 60:
        return f"{seconds:.2f} s"
    else:
        minutes = int(seconds // 60)
        secs = seconds % 60
        return f"{minutes}m {secs:.2f}s"


def run_iouring_benchmark(
    size: int,
    buffer_size: int,
    buffer_count: int,
    output_path: str,
    queue_depth: int = 256,
    use_odirect: bool = True,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0
) -> dict:
    """
    Run benchmark with io_uring for batched I/O.
    
    Strategy:
    1. Open file with O_DIRECT
    2. Create io_uring ring
    3. Generate data in buffers, submit writes in batches
    4. Wait for all completions
    5. Measure ONLY write submission + completion time
    """
    print("\n" + "="*70)
    print("io_uring STORAGE BENCHMARK")
    print("="*70)
    print(f"Total size:     {format_bytes(size)}")
    print(f"Buffer size:    {format_bytes(buffer_size)}")
    print(f"Buffer count:   {buffer_count}")
    print(f"Pool size:      {format_bytes(buffer_size * buffer_count)}")
    print(f"Queue depth:    {queue_depth}")
    print(f"Output file:    {output_path}")
    print(f"O_DIRECT:       {'✓ Enabled' if use_odirect else '⚠ Disabled'}")
    print("="*70)
    
    # Validate alignment
    if use_odirect and (buffer_size % 4096 != 0):
        print(f"\n⚠ Warning: Buffer size {buffer_size} not 4KB-aligned, disabling O_DIRECT")
        use_odirect = False
    
    results = {}
    total_buffers = (size + buffer_size - 1) // buffer_size
    
    # Phase 1: Allocate buffers
    print(f"\nPhase 1: Allocating {buffer_count} buffers...")
    alloc_start = time.perf_counter()
    buffers = [create_aligned_buffer(buffer_size) for _ in range(buffer_count)]
    alloc_time = time.perf_counter() - alloc_start
    print(f"  ✓ Allocated {format_bytes(buffer_size * buffer_count)} in {format_duration(alloc_time)}")
    
    # Phase 2: Open file
    print(f"\nPhase 2: Opening file with O_DIRECT...")
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    if use_odirect:
        try:
            flags |= os.O_DIRECT
        except AttributeError:
            print("  ⚠ O_DIRECT not available")
            use_odirect = False
    
    fd = os.open(output_path, flags, 0o644)
    print(f"  ✓ File opened (fd={fd}, O_DIRECT={'yes' if use_odirect else 'no'})")
    
    # Phase 3: Initialize io_uring
    print(f"\nPhase 3: Initializing io_uring (queue_depth={queue_depth})...")
    ring = uring.io_uring()
    ret = uring.io_uring_queue_init(queue_depth, ring, 0)
    if ret < 0:
        os.close(fd)
        raise OSError(f"io_uring_queue_init failed: {os.strerror(-ret)}")
    print(f"  ✓ io_uring initialized")
    
    # Phase 4: Generate and Write (TIMED)
    print(f"\nPhase 4: Writing {total_buffers} buffers with io_uring...")
    gen = dgen_py.Generator(size=size, dedup_ratio=dedup_ratio, compress_ratio=compress_ratio)
    
    write_start = time.perf_counter()
    bytes_written = 0
    offset = 0
    submitted = 0
    completed = 0
    
    try:
        # Submit all writes
        for i in range(total_buffers):
            buf_idx = i % buffer_count
            buffer = buffers[buf_idx]
            
            # Generate data
            gen.fill_chunk(buffer)
            
            # Calculate write size
            write_size = min(buffer_size, size - offset)
            
            # Get submission queue entry
            sqe = uring.io_uring_get_sqe(ring)
            while sqe is None:
                # Queue full - submit and wait for some completions
                uring.io_uring_submit(ring)
                cqe_ptr = uring.io_uring_cqe()
                ret = uring.io_uring_wait_cqe(ring, cqe_ptr)
                if ret == 0:
                    cqe = cqe_ptr.value if hasattr(cqe_ptr, 'value') else cqe_ptr
                    if cqe.res > 0:
                        bytes_written += cqe.res
                    completed += 1
                    uring.io_uring_cqe_seen(ring, cqe)
                sqe = uring.io_uring_get_sqe(ring)
            
            # Prepare write operation
            uring.io_uring_prep_write(sqe, fd, buffer.obj, write_size, offset)
            uring.io_uring_sqe_set_data64(sqe, i)
            offset += write_size
            submitted += 1
        
        # Submit final batch
        uring.io_uring_submit(ring)
        
        # Wait for all completions
        while completed < submitted:
            cqe_ptr = uring.io_uring_cqe()
            ret = uring.io_uring_wait_cqe(ring, cqe_ptr)
            if ret == 0:
                cqe = cqe_ptr.value if hasattr(cqe_ptr, 'value') else cqe_ptr
                if cqe.res > 0:
                    bytes_written += cqe.res
                elif cqe.res < 0:
                    print(f"  ⚠ Write error: {os.strerror(-cqe.res)}")
                completed += 1
                uring.io_uring_cqe_seen(ring, cqe)
        
        # Fsync
        os.fsync(fd)
        write_time = time.perf_counter() - write_start
        
    finally:
        uring.io_uring_queue_exit(ring)
        os.close(fd)
    
    write_throughput = bytes_written / write_time / (1024**3)
    
    print(f"  ✓ Wrote {format_bytes(bytes_written)} in {format_duration(write_time)}")
    print(f"    Throughput:  {write_throughput:.2f} GB/s")
    print(f"    Operations:  {submitted}")
    print(f"    Completions: {completed}")
    
    # Results
    print("\n" + "="*70)
    print("RESULTS")
    print("="*70)
    print(f"\n⭐ STORAGE WRITE PERFORMANCE: {write_throughput:.2f} GB/s")
    print(f"   (Batched {submitted} operations via io_uring)")
    print(f"\nWrite time:          {format_duration(write_time)}")
    print(f"Data written:        {format_bytes(bytes_written)}")
    print(f"I/O operations:      {submitted}")
    print(f"I/O mode:            {'✓ O_DIRECT' if use_odirect else '⚠ Buffered'}")
    print(f"\nOverhead (included in write time):")
    print(f"  - Allocation:      {format_duration(alloc_time)} (setup only)")
    print(f"  - Data generation: (overlapped with submission)")
    print(f"\nMemory footprint:    {format_bytes(buffer_size * buffer_count)}")
    print("="*70)
    
    results['write_time'] = write_time
    results['write_throughput'] = write_throughput
    results['bytes_written'] = bytes_written
    results['operations'] = submitted
    results['used_odirect'] = use_odirect
    
    return results


def parse_size(size_str: str) -> int:
    """Parse size string like '2GB', '500MB' to bytes."""
    size_str = size_str.upper().strip()
    
    multipliers = {
        'B': 1,
        'K': 1024,
        'KB': 1024,
        'M': 1024**2,
        'MB': 1024**2,
        'G': 1024**3,
        'GB': 1024**3,
        'T': 1024**4,
        'TB': 1024**4,
    }
    
    num_part = ''
    unit_part = ''
    for i, c in enumerate(size_str):
        if c.isdigit() or c == '.':
            num_part += c
        else:
            unit_part = size_str[i:].strip()
            break
    
    if not num_part:
        raise ValueError(f"Invalid size: {size_str}")
    
    num = float(num_part)
    
    if not unit_part:
        return int(num)
    
    if unit_part not in multipliers:
        raise ValueError(f"Unknown unit: {unit_part}")
    
    size_bytes = int(num * multipliers[unit_part])
    
    # Round up to 4KB boundary
    page_size = 4096
    if size_bytes % page_size != 0:
        size_bytes = ((size_bytes + page_size - 1) // page_size) * page_size
    
    return size_bytes


def main():
    parser = argparse.ArgumentParser(
        description="io_uring storage benchmark - eliminate syscall overhead",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # 2 GiB test with io_uring (should match single buffer: ~1.26 GB/s)
  python3 iouring_benchmark.py --size 2GB --output /mnt/nvme/test.bin
  
  # Large test with custom buffer pool
  python3 iouring_benchmark.py --size 100GB --buffer-size 8MB --buffer-count 256 \\
      --output /mnt/nvme/test.bin
  
  # Adjust queue depth (default 256)
  python3 iouring_benchmark.py --size 10GB --queue-depth 512 --output /mnt/nvme/test.bin
"""
    )
    
    parser.add_argument('--size', '-s', type=str, default='2GB',
                        help='Total data size (e.g., 2GB, 10GB, 1TB)')
    parser.add_argument('--output', '-o', type=str, default='iouring_test.bin',
                        help='Output file path')
    parser.add_argument('--buffer-size', type=str, default='8MB',
                        help='Size of each buffer (must be 4KB multiple)')
    parser.add_argument('--buffer-count', type=int, default=256,
                        help='Number of buffers in pool')
    parser.add_argument('--queue-depth', type=int, default=256,
                        help='io_uring submission queue depth')
    parser.add_argument('--no-direct', action='store_true',
                        help='Disable O_DIRECT (use buffered I/O)')
    parser.add_argument('--dedup-ratio', type=float, default=1.0,
                        help='Deduplication ratio (1.0 = no dedup)')
    parser.add_argument('--compress-ratio', type=float, default=1.0,
                        help='Compression ratio (1.0 = incompressible)')
    
    args = parser.parse_args()
    
    # Parse sizes
    try:
        size = parse_size(args.size)
        buffer_size = parse_size(args.buffer_size)
    except ValueError as e:
        print(f"Error: {e}")
        sys.exit(1)
    
    # Check output directory
    output_path = Path(args.output)
    if not output_path.parent.exists():
        print(f"Error: Directory {output_path.parent} does not exist")
        sys.exit(1)
    
    # Run benchmark
    try:
        results = run_iouring_benchmark(
            size=size,
            buffer_size=buffer_size,
            buffer_count=args.buffer_count,
            output_path=str(output_path),
            queue_depth=args.queue_depth,
            use_odirect=not args.no_direct,
            dedup_ratio=args.dedup_ratio,
            compress_ratio=args.compress_ratio
        )
        
        print(f"\nOutput file: {output_path}")
        print(f"File size:   {format_bytes(output_path.stat().st_size)}")
        
    except KeyboardInterrupt:
        print("\n\nBenchmark interrupted by user")
        sys.exit(1)
    except Exception as e:
        print(f"\nError: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)


if __name__ == '__main__':
    main()
