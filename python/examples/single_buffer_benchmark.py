#!/usr/bin/env python3
"""
Single Buffer Benchmark - No Streaming

Simple test that:
1. Allocates ONE large buffer (2 GiB)
2. Fills it with dgen-py generated data
3. Writes it out with O_DIRECT
4. Reports performance

Compare to storage_benchmark.py to see streaming overhead.
"""

import os
import sys
import time
import mmap
import argparse
from pathlib import Path

try:
    import dgen_py
except ImportError:
    print("Error: dgen-py not installed")
    print("Install with: pip install dgen-py")
    sys.exit(1)


def create_aligned_buffer(size: int) -> memoryview:
    """Create page-aligned buffer for O_DIRECT."""
    # Allocate anonymous mmap (page-aligned by default)
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


def run_single_buffer_benchmark(
    size: int,
    output_path: str,
    use_odirect: bool = True,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0
) -> dict:
    """
    Run benchmark with single large buffer.
    
    Args:
        size: Total size in bytes (must be multiple of 4096 for O_DIRECT)
        output_path: Output file path
        use_odirect: Use O_DIRECT for unbuffered I/O
        dedup_ratio: Deduplication ratio (1.0 = no dedup)
        compress_ratio: Compression ratio (1.0 = incompressible)
    
    Returns:
        Dictionary with performance results
    """
    print("\n" + "="*70)
    print("SINGLE BUFFER BENCHMARK - No Streaming")
    print("="*70)
    print(f"Buffer size:    {format_bytes(size)}")
    print(f"Output file:    {output_path}")
    print(f"O_DIRECT:       {'✓ Enabled' if use_odirect else '⚠ Disabled (buffered I/O)'}")
    print(f"Dedup ratio:    {dedup_ratio:.2f}")
    print(f"Compress ratio: {compress_ratio:.2f}")
    print("="*70)
    
    # Validate alignment for O_DIRECT
    if use_odirect and size % 4096 != 0:
        print(f"\n⚠ Warning: Size {size} not 4KB-aligned, disabling O_DIRECT")
        use_odirect = False
    
    results = {}
    
    # Phase 1: Allocate buffer
    print("\nPhase 1: Allocating buffer...")
    alloc_start = time.perf_counter()
    buffer = create_aligned_buffer(size)
    alloc_time = time.perf_counter() - alloc_start
    print(f"  ✓ Allocated {format_bytes(size)} in {format_duration(alloc_time)}")
    results['alloc_time'] = alloc_time
    
    # Phase 2: Generate data
    print("\nPhase 2: Generating data...")
    gen = dgen_py.Generator(
        size=size,
        dedup_ratio=dedup_ratio,
        compress_ratio=compress_ratio
    )
    
    gen_start = time.perf_counter()
    gen.fill_chunk(buffer)
    gen_time = time.perf_counter() - gen_start
    gen_throughput = size / gen_time / (1024**3)  # GB/s
    
    print(f"  ✓ Generated {format_bytes(size)} in {format_duration(gen_time)}")
    print(f"    Throughput: {gen_throughput:.2f} GB/s")
    results['gen_time'] = gen_time
    results['gen_throughput'] = gen_throughput
    
    # Phase 3: Write to disk
    print("\nPhase 3: Writing to disk...")
    
    # Open file with O_DIRECT if requested
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    if use_odirect:
        try:
            flags |= os.O_DIRECT
        except AttributeError:
            print("  ⚠ O_DIRECT not available on this platform")
            use_odirect = False
    
    write_start = time.perf_counter()
    
    fd = os.open(output_path, flags, 0o644)
    try:
        # Single write operation
        bytes_written = os.write(fd, buffer)
        os.fsync(fd)  # Ensure data is on disk
        
        write_time = time.perf_counter() - write_start
        write_throughput = size / write_time / (1024**3)  # GB/s
        
        if bytes_written != size:
            print(f"  ⚠ Warning: Expected {size} bytes, wrote {bytes_written}")
        
        print(f"  ✓ Wrote {format_bytes(bytes_written)} in {format_duration(write_time)}")
        print(f"    Throughput: {write_throughput:.2f} GB/s")
        print(f"    I/O mode: {'✓ O_DIRECT' if use_odirect else '⚠ Buffered'}")
        
        results['write_time'] = write_time
        results['write_throughput'] = write_throughput
        results['bytes_written'] = bytes_written
        results['used_odirect'] = use_odirect
        
    finally:
        os.close(fd)
    
    # Phase 4: Results
    total_time = alloc_time + gen_time + write_time
    
    print("\n" + "="*70)
    print("RESULTS")
    print("="*70)
    print(f"\n⭐ STORAGE WRITE PERFORMANCE: {write_throughput:.2f} GB/s")
    print(f"   (This is the ONLY metric that matters for storage)")
    print(f"\nWrite time:          {format_duration(write_time)}")
    print(f"Data written:        {format_bytes(size)}")
    print(f"I/O mode:            {'✓ O_DIRECT' if use_odirect else '⚠ Buffered'}")
    print(f"\nOverhead (not counted in storage perf):")
    print(f"  - Allocation:      {format_duration(alloc_time)}")
    print(f"  - Data generation: {format_duration(gen_time)} ({gen_throughput:.2f} GB/s)")
    print(f"\nTotal elapsed time:  {format_duration(total_time)}")
    print("="*70)
    
    results['total_time'] = total_time
    
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
    
    # Find where digits end
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
        # Assume bytes
        return int(num)
    
    if unit_part not in multipliers:
        raise ValueError(f"Unknown unit: {unit_part}")
    
    size_bytes = int(num * multipliers[unit_part])
    
    # Round up to 4KB boundary for O_DIRECT
    page_size = 4096
    if size_bytes % page_size != 0:
        size_bytes = ((size_bytes + page_size - 1) // page_size) * page_size
    
    return size_bytes


def main():
    parser = argparse.ArgumentParser(
        description="Single buffer benchmark - no streaming",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Simple 2 GiB test with O_DIRECT
  python3 single_buffer_benchmark.py --size 2GB --output /mnt/nvme/test.bin
  
  # Test without O_DIRECT (buffered I/O)
  python3 single_buffer_benchmark.py --size 2GB --output /tmp/test.bin --no-direct
  
  # 4 GiB test with dedup/compression
  python3 single_buffer_benchmark.py --size 4GB --output /mnt/nvme/test.bin \\
      --dedup-ratio 2.0 --compress-ratio 1.5
"""
    )
    
    parser.add_argument('--size', '-s', type=str, default='2GB',
                        help='Buffer size (e.g., 2GB, 500MB, 1TB)')
    parser.add_argument('--output', '-o', type=str, default='single_buffer_test.bin',
                        help='Output file path')
    parser.add_argument('--no-direct', action='store_true',
                        help='Disable O_DIRECT (use buffered I/O)')
    parser.add_argument('--dedup-ratio', type=float, default=1.0,
                        help='Deduplication ratio (1.0 = no dedup)')
    parser.add_argument('--compress-ratio', type=float, default=1.0,
                        help='Compression ratio (1.0 = incompressible)')
    
    args = parser.parse_args()
    
    # Parse size
    try:
        size = parse_size(args.size)
    except ValueError as e:
        print(f"Error: {e}")
        sys.exit(1)
    
    # Check if output directory exists
    output_path = Path(args.output)
    if not output_path.parent.exists():
        print(f"Error: Directory {output_path.parent} does not exist")
        sys.exit(1)
    
    # Run benchmark
    try:
        results = run_single_buffer_benchmark(
            size=size,
            output_path=str(output_path),
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
