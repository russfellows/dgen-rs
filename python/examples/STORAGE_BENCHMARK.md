# Storage Benchmark Example

High-performance storage write benchmark using dgen-py with producer-consumer pipeline.

## Overview

This example demonstrates how to use dgen-py to create a storage benchmark that:

1. **Generates data at CPU speed** using dgen-py (5-15 GB/s per core)
2. **Writes to storage in parallel** using a producer-consumer pipeline
3. **Uses O_DIRECT** to bypass page cache for realistic storage performance
4. **Reports detailed metrics** including throughput, latency, and bottleneck analysis

## Architecture

```
┌─────────────┐         ┌──────────────┐         ┌──────────────┐
│  Producer   │   -->   │ Buffer Pool  │   -->   │  Consumer    │
│  (dgen-py)  │         │ (256 x 4MB)  │         │  (Storage)   │
└─────────────┘         └──────────────┘         └──────────────┘
   5-15 GB/s              Page-aligned               3-7 GB/s
   CPU-bound              Zero-copy                  I/O-bound
```

### Key Design Decisions

1. **Double Buffering**: Uses a pool of buffers (default: 250 x 4MB = 1GB) to ensure storage never waits for data generation

2. **Page-Aligned Buffers**: Uses `mmap` to create buffers aligned to page boundaries (4KB), required for O_DIRECT

3. **O_DIRECT Mode**: Bypasses Linux page cache to:
   - Get realistic storage performance (not cached writes)
   - Prevent memory pressure from large files
   - Enable true Direct Memory Access (DMA)

4. **Zero-Copy Generation**: Uses dgen-py's `fill_chunk()` API to generate data directly into buffers (no memcpy)

## Usage

### Basic Test (10 GB)
```bash
python storage_benchmark.py --size 10GB --output /mnt/nvme/test.bin
```

### Large Test with Custom Buffers
```bash
# 100 GB with 256 x 8MB buffers (2GB pool)
python storage_benchmark.py \
    --size 100GB \
    --buffer-size 8MB \
    --buffer-count 256 \
    --output /mnt/nvme/test.bin
```

### Test with Compression/Deduplication
```bash
# 50 GB of 3:1 compressible, 2:1 dedup data
python storage_benchmark.py \
    --size 50GB \
    --compress-ratio 3.0 \
    --dedup-ratio 2.0 \
    --output /mnt/nvme/test.bin
```

### Quick Test (Without O_DIRECT)
```bash
# 1 GB test using buffered I/O (for unsupported filesystems)
python storage_benchmark.py \
    --size 1GB \
    --no-direct \
    --output /tmp/test.bin
```

## Command Line Options

| Option | Default | Description |
|--------|---------|-------------|
| `--size` / `-s` | 10GB | Total data size (e.g., 10GB, 500MB, 1TB) |
| `--output` / `-o` | benchmark_output.bin | Output file path |
| `--buffer-size` | 4MB | Size of each buffer (must be multiple of 4KB) |
| `--buffer-count` | 250 | Number of buffers in pool |
| `--dedup-ratio` | 1.0 | Deduplication ratio (1.0 = no dedup) |
| `--compress-ratio` | 1.0 | Compression ratio (1.0 = incompressible) |
| `--no-direct` | false | Disable O_DIRECT (use buffered I/O) |
| `--numa-mode` | auto | NUMA optimization (auto/force/disabled) |
| `--max-threads` | auto | Max threads for generation |

## Example Output

```
======================================================================
HIGH-PERFORMANCE STORAGE BENCHMARK
======================================================================

Configuration:
  Total size:      10.00 GB
  Buffer size:     4.00 MB
  Buffer count:    250
  Total pool:      1.00 GB
  Output file:     /mnt/nvme/test.bin
  Dedup ratio:     1.0:1
  Compress ratio:  1.0:1
  Direct I/O:      True
  NUMA mode:       auto

System:
  NUMA nodes:      1
  CPUs:            16

======================================================================

Allocating 250 aligned buffers...
Starting producer and consumer threads...

[Producer] Generated: 10.00 GB (100.0%) @ 8.42 GB/s
[Producer] Complete: 10.00 GB generated

[Consumer] Written: 10.00 GB @ 3.25 GB/s (lat: 1.23 ms)
[Consumer] Complete: 10.00 GB written (2560 writes)

======================================================================
RESULTS
======================================================================

Data Transfer:
  Generated:       10.00 GB
  Written:         10.00 GB
  Total time:      3.08 s

Throughput:
  Write:           3.25 GB/s
  Generation:      3.25 GB/s

Latency:
  Avg write:       1.20 ms
  Write count:     2560

Utilization:
  Producer:        38.2% (waiting: 1.90s)
  Consumer:        95.3% (waiting: 0.14s)

Bottleneck Analysis:
  ⚠ Producer is waiting (38.2% util)
    → Consumer is slower than generator
    → Storage is the bottleneck

======================================================================
```

## Performance Analysis

### Interpreting Results

1. **Throughput**: The "Write" throughput shows actual storage performance
   - Gen4 NVMe: 3-7 GB/s expected
   - SATA SSD: 0.5-0.6 GB/s expected
   - HDD: 0.1-0.2 GB/s expected

2. **Utilization**:
   - **Producer waiting** → Storage bottleneck (expected for fast NVMe)
   - **Consumer waiting** → CPU/generation bottleneck (shouldn't happen with dgen-py!)
   - **Both >80%** → Balanced pipeline

3. **Latency**:
   - Shows average time per write operation
   - Lower is better (typically 0.5-2ms for NVMe)

### Tuning

If storage throughput is lower than expected:

1. **Increase buffer size**: Try 8MB or 16MB buffers
   ```bash
   --buffer-size 8MB
   ```

2. **Increase buffer count**: Larger pool = more buffering
   ```bash
   --buffer-count 512
   ```

3. **Check alignment**: Buffer size should be multiple of 4096
   ```bash
   --buffer-size 8388608  # Exactly 8MB
   ```

4. **Verify O_DIRECT**: Check that O_DIRECT is enabled in output
   - If filesystem doesn't support it, try different mount or `--no-direct`

## Platform Notes

### Linux
- **Recommended**: Full O_DIRECT support with page-aligned buffers
- Requires filesystem that supports O_DIRECT (ext4, xfs, btrfs)

### macOS / Windows
- Falls back to buffered I/O (O_DIRECT not available)
- Still useful for testing, but throughput will hit page cache
- Use `--no-direct` flag explicitly

## Integration with Storage Testing

### Use Cases

1. **NVMe Performance Testing**: Measure real storage write bandwidth
2. **Storage Array Benchmarking**: Test distributed storage systems
3. **Network Storage**: Test NFS/SMB write performance
4. **Cloud Storage**: Test object storage upload speeds (with file backend)

### Extending the Example

To test other storage types, modify the `consumer_thread()` function:

```python
# Example: Upload to S3 instead of local file
def consumer_thread_s3(...):
    import boto3
    s3 = boto3.client('s3')
    
    while True:
        item = full_buffers.get()
        if item is None:
            break
        
        buf, nbytes = item
        
        # Upload to S3
        s3.put_object(
            Bucket='my-bucket',
            Key=f'chunk_{total_written}',
            Body=buf[:nbytes]
        )
        
        total_written += nbytes
        empty_buffers.put(buf)
```

## Troubleshooting

### "Operation not supported" error
- Filesystem doesn't support O_DIRECT
- Solution: Use `--no-direct` or test on different filesystem

### Low throughput (<1 GB/s on NVMe)
- Check buffer alignment (should be 4KB multiple)
- Increase buffer size: `--buffer-size 8MB`
- Verify no other I/O happening on device
- Check with `iostat -x 1` during test

### Producer utilization >95%, Consumer waiting
- **Very rare** with dgen-py (generates at >5 GB/s)
- Possible if using very fast storage array (>10 GB/s)
- Increase generation threads: `--max-threads 32`

### Out of memory
- Reduce buffer pool: `--buffer-count 100`
- Reduce buffer size: `--buffer-size 2MB`
- Total pool size = buffer_size × buffer_count

## Related Examples

- `zero_copy_demo.py` - Demonstrates dgen-py zero-copy API
- `quick_perf_test.py` - Find optimal dgen-py settings for your system
- `benchmark_cpu_numa.py` - NUMA performance testing

## References

- [dgen-py Documentation](../README.md)
- [O_DIRECT Linux Man Page](https://man7.org/linux/man-pages/man2/open.2.html)
- [Producer-Consumer Pattern](https://en.wikipedia.org/wiki/Producer%E2%80%93consumer_problem)
