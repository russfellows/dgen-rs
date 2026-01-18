# Streaming Performance Limitation Issue

**Date**: January 9, 2026  
**Status**: Documented, Not Implemented  
**Impact**: Streaming mode achieves only 60% of storage write capability

## Problem Summary

When writing large files (>100GB) that exceed available memory, we must use streaming mode (generate small chunks → write → repeat). However, this approach achieves only **0.75-0.80 GB/s** versus **1.26-1.38 GB/s** with single-buffer writes.

## Root Cause Analysis

### Performance Breakdown (2 GB test file on NVMe)

| Approach | Write Perf | Method | Limitation |
|----------|------------|--------|------------|
| Single buffer | 1.26 GB/s | Generate ALL (2GB), then write ALL | ❌ Requires full file in memory |
| Pre-generated + io_uring | 1.38 GB/s | Generate 256 buffers, then write via io_uring | ❌ Still requires full file in memory |
| Threading (8 writers) | 0.75 GB/s | Producer-consumer with queue.Queue | ✅ Streaming, but slow |
| io_uring inline gen | 0.60 GB/s | Generate then submit in Python loop | ✅ Streaming, but slower |

### Hardware Capabilities (Measured)

- **Storage write limit**: 1.28-1.38 GB/s (O_DIRECT, NVMe)
- **Data generation (Rust)**: 2.13 GB/s (standalone)
- **Data generation (with concurrent I/O)**: 1.41 GB/s (33% slower due to contention)

### The Python Bottleneck

When generation and I/O run concurrently in Python:

```python
# Python loop overhead kills performance
for i in range(256):  # 256 iterations = 256 Python→Rust→Python transitions
    gen.fill_chunk(buffer)  # Rust call (3.7ms)
    os.write(fd, buffer)    # Syscall (6.5ms)
    # Total: 10.2ms per iteration
    # Result: 8MB / 10.2ms = 0.78 GB/s
```

**Issues:**
1. **Memory contention**: Generation and I/O fight for memory bandwidth
2. **Python loop overhead**: 256 iterations with GIL acquisition/release
3. **Thread synchronization**: queue.Queue locking overhead
4. **Serialization**: Even with threading, operations partially serialize

### Proof of Python Overhead

| Test | Result | Conclusion |
|------|--------|------------|
| Pure generation (Python loop) | 2.13 GB/s | ✅ Rust is fast |
| Pure I/O (Python loop) | 1.28 GB/s | ✅ I/O is at hardware limit |
| Generation + I/O (threading) | 0.79 GB/s | ❌ Only 1.57x parallelism (should be ~2x) |
| Generation + I/O (io_uring) | 0.60 GB/s | ❌ Even worse (inline generation) |

**Expected with perfect overlap**: min(2.13, 1.28) = 1.28 GB/s  
**Actual result**: 0.75-0.80 GB/s  
**Performance lost to Python overhead**: 38%

## Proposed Solution: Rust-Native Write Function

Add a new function to `dgen-py` that performs **both generation and writing entirely in Rust**:

```python
# Current approach (slow - 0.75 GB/s)
gen = dgen_py.Generator(size=1024**4)
fd = os.open("output.bin", os.O_WRONLY|os.O_CREAT|os.O_DIRECT)
for i in range(thousands):
    gen.fill_chunk(buffer)
    os.write(fd, buffer)

# Proposed approach (fast - 1.28+ GB/s)
stats = dgen_py.write_file(
    path="output.bin",
    size=1024**4,              # 1 TiB
    buffer_size=8*1024**2,     # 8 MB
    buffer_count=256,          # 2 GB pool
    use_direct_io=True,
    dedup_ratio=1.0,
    compress_ratio=1.0,
    use_io_uring=True,         # Linux only
)
```

### Implementation Outline (Rust)

```rust
// In dgen-rs/src/lib.rs

#[pyfunction]
#[pyo3(signature = (path, size, buffer_size=8*1024*1024, buffer_count=256, use_direct_io=true, dedup_ratio=1.0, compress_ratio=1.0, use_io_uring=true))]
fn write_file(
    path: &str,
    size: u64,
    buffer_size: usize,
    buffer_count: usize,
    use_direct_io: bool,
    dedup_ratio: f64,
    compress_ratio: f64,
    use_io_uring: bool,
) -> PyResult<WriteStats> {
    // 1. Open file with O_DIRECT (if requested)
    let mut flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC;
    if use_direct_io {
        flags |= OFlag::O_DIRECT;
    }
    let fd = open(path, flags, Mode::S_IRUSR | Mode::S_IWUSR)?;
    
    // 2. Allocate page-aligned buffer pool
    let buffers = allocate_aligned_buffers(buffer_size, buffer_count)?;
    
    // 3. Initialize generator
    let mut generator = Generator::new(size, dedup_ratio, compress_ratio);
    
    // 4. Choose I/O strategy
    let stats = if use_io_uring && cfg!(target_os = "linux") {
        write_with_io_uring(fd, &mut generator, &buffers, size, buffer_size)?
    } else {
        write_with_pwrite(fd, &mut generator, &buffers, size, buffer_size)?
    };
    
    // 5. Cleanup
    nix::unistd::fsync(fd)?;
    nix::unistd::close(fd)?;
    
    Ok(stats)
}

// io_uring implementation (Linux)
fn write_with_io_uring(
    fd: RawFd,
    generator: &mut Generator,
    buffers: &[AlignedBuffer],
    total_size: u64,
    buffer_size: usize,
) -> Result<WriteStats> {
    let mut ring = IoUring::<Entry>::new(256)?;
    let num_buffers = (total_size + buffer_size as u64 - 1) / buffer_size as u64;
    
    let mut offset = 0u64;
    let mut submitted = 0;
    let mut completed = 0;
    
    // Main loop - ALL IN RUST, NO PYTHON!
    for i in 0..num_buffers {
        let buf_idx = (i as usize) % buffers.len();
        
        // Generate data (Rayon parallel)
        generator.fill_chunk(&mut buffers[buf_idx])?;
        
        // Submit write
        let write_size = std::cmp::min(buffer_size, (total_size - offset) as usize);
        unsafe {
            ring.submission()
                .push(
                    opcode::Write::new(Fd(fd), buffers[buf_idx].as_ptr(), write_size as u32)
                        .offset(offset)
                        .build()
                        .user_data(i)
                )?;
        }
        submitted += 1;
        offset += write_size as u64;
        
        // Reap completions periodically
        if submitted - completed >= 32 {
            ring.submit_and_wait(1)?;
            for cqe in &mut ring.completion() {
                completed += 1;
            }
        }
    }
    
    // Wait for all completions
    ring.submit()?;
    while completed < submitted {
        ring.submit_and_wait(1)?;
        for cqe in &mut ring.completion() {
            if cqe.result() < 0 {
                return Err(Error::IoError(cqe.result()));
            }
            completed += 1;
        }
    }
    
    Ok(WriteStats {
        bytes_written: total_size,
        operations: submitted,
        duration_secs: /* measure */,
    })
}

// Fallback for non-Linux or when io_uring disabled
fn write_with_pwrite(
    fd: RawFd,
    generator: &mut Generator,
    buffers: &[AlignedBuffer],
    total_size: u64,
    buffer_size: usize,
) -> Result<WriteStats> {
    let num_buffers = (total_size + buffer_size as u64 - 1) / buffer_size as u64;
    let mut offset = 0i64;
    
    for i in 0..num_buffers {
        let buf_idx = (i as usize) % buffers.len();
        
        // Generate data
        generator.fill_chunk(&mut buffers[buf_idx])?;
        
        // Write with pwrite (positioned write, thread-safe)
        let write_size = std::cmp::min(buffer_size, (total_size - offset as u64) as usize);
        nix::sys::uio::pwrite(fd, &buffers[buf_idx][..write_size], offset)?;
        offset += write_size as i64;
    }
    
    Ok(WriteStats { /* ... */ })
}
```

### Expected Performance

- **Current (Python streaming)**: 0.75-0.80 GB/s
- **With Rust write_file()**: 1.28-1.38 GB/s (storage hardware limit)
- **Improvement**: 60-70% faster
- **Memory footprint**: Same (configurable buffer pool, e.g., 2 GB)

### Dependencies Needed

```toml
# In dgen-rs/Cargo.toml
[dependencies]
io-uring = { version = "0.7", optional = true }  # Linux only
nix = { version = "0.29", features = ["fs", "mman"] }

[features]
io_uring = ["dep:io-uring"]
```

### Python API Usage

```python
import dgen_py

# Simple usage
dgen_py.write_file(
    path="/mnt/nvme/test.bin",
    size=1024**4,  # 1 TiB
)

# Advanced usage
stats = dgen_py.write_file(
    path="/data/training_data.bin",
    size=500 * 1024**3,     # 500 GB
    buffer_size=16*1024**2,  # 16 MB buffers
    buffer_count=128,        # 2 GB pool
    use_direct_io=True,
    dedup_ratio=2.0,
    compress_ratio=1.5,
    use_io_uring=True,
)

print(f"Wrote {stats.bytes_written} bytes in {stats.duration_secs:.2f}s")
print(f"Throughput: {stats.bytes_written / stats.duration_secs / 1e9:.2f} GB/s")
```

## Benefits

1. **60-70% performance improvement** for streaming writes
2. **Scales to arbitrary file sizes** (1 TiB+) with fixed memory (2-4 GB)
3. **Zero Python overhead** - entire write loop in Rust
4. **io_uring support** - optimal Linux I/O performance
5. **Cross-platform** - falls back to pwrite() on non-Linux

## Trade-offs

1. **Increased complexity** in dgen-rs
2. **Platform-specific code** (io_uring Linux-only)
3. **Less flexible** than Python loop (but much faster)
4. **Adds ~500 lines** to Rust codebase

## Workaround (Current)

For now, users needing >1 GB/s must use single-buffer approach with sufficient RAM:

```python
# For 100 GB file, need 100 GB RAM
size = 100 * 1024**3
buffer = mmap.mmap(-1, size, mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)

gen = dgen_py.Generator(size=size)
gen.fill_chunk(memoryview(buffer))

fd = os.open("output.bin", os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_DIRECT)
os.write(fd, buffer)
os.fsync(fd)
os.close(fd)
# Achieves 1.26 GB/s
```

## Future Considerations

1. **Read support**: `dgen_py.read_and_validate()` for verification
2. **Progress callbacks**: Python callback for progress updates
3. **Multi-file support**: Write multiple files in parallel
4. **NUMA awareness**: Pin buffers to NUMA nodes

## References

- Test results: `/home/eval/Documents/Code/dgen-rs/python/examples/`
- Performance benchmarks: `storage_benchmark.py`, `single_buffer_benchmark.py`, `iouring_benchmark.py`
- Issue discovered: January 9, 2026 during streaming performance investigation
