# CPU and NUMA Control Guide

## Overview

dgen-rs provides fine-grained control over CPU thread usage and NUMA (Non-Uniform Memory Access) optimizations for maximum performance on both UMA (single-socket) and NUMA (multi-socket) systems.

## Thread Count Control

### Rust API

```rust
use dgen_rs::{GeneratorConfig, NumaMode};

// Use all available CPU cores (default)
let config = GeneratorConfig {
    size: 100 * 1024 * 1024,
    dedup_factor: 1,
    compress_factor: 1,
    numa_mode: NumaMode::Auto,
    max_threads: None,  // Detects and uses all cores
};

// Limit to 8 threads
let config = GeneratorConfig {
    max_threads: Some(8),
    ..Default::default()
};

// Single-threaded (for baseline measurements)
let config = GeneratorConfig {
    max_threads: Some(1),
    ..Default::default()
};
```

### Python API

```python
import dgen_py

# Use all cores (default)
data = dgen_py.generate_data(100 * 1024 * 1024)

# Limit to 8 threads
data = dgen_py.generate_data(100 * 1024 * 1024, max_threads=8)

# Single-threaded
data = dgen_py.generate_data(100 * 1024 * 1024, max_threads=1)

# Zero-copy into buffer with thread control
buf = bytearray(100 * 1024 * 1024)
dgen_py.fill_buffer(buf, max_threads=4)
```

### Performance Scaling

Typical performance on 12-core system:

| Threads | Throughput | Efficiency |
|---------|------------|------------|
| 1       | 1.4 GB/s   | 100% (baseline) |
| 4       | 5.9 GB/s   | 105% (linear scaling) |
| 8       | 11.2 GB/s  | 100% |
| 12      | 13.6 GB/s  | 81% (memory bandwidth limit) |

**Note**: Efficiency drops at high core counts due to memory bandwidth saturation, not code inefficiency.

## NUMA Mode Control

### What is NUMA?

```
UMA (Uniform Memory Access)        NUMA (Non-Uniform Memory Access)
┌─────────────┐                   ┌─────────────┐   ┌─────────────┐
│   CPU       │                   │  CPU Node 0 │   │  CPU Node 1 │
│  (1 socket) │                   │  (Socket 0) │   │  (Socket 1) │
├─────────────┤                   ├─────────────┤   ├─────────────┤
│   Memory    │                   │  Memory 0   │   │  Memory 1   │
│   128 GB    │                   │   128 GB    │   │   128 GB    │
└─────────────┘                   └──────┬──────┘   └──────┬──────┘
                                         └──────────────────┘
                                         Cross-node traffic
                                         (high latency)

Examples:                          Examples:
- AWS c5.large                     - 2-socket Xeon servers
- Laptop/workstation               - 2-socket EPYC servers
- Single-socket servers            - Large bare-metal instances
```

### NUMA Modes

#### `NumaMode::Auto` (Default)
- **Behavior**: Enable NUMA optimizations only on detected multi-node systems
- **Use case**: Default for most applications (safe auto-detection)
- **Example**:
  ```rust
  let config = GeneratorConfig {
      numa_mode: NumaMode::Auto,
      ..Default::default()
  };
  ```

#### `NumaMode::Force`
- **Behavior**: Enable NUMA optimizations even on UMA systems
- **Use case**: Testing, validation, or overriding auto-detection
- **Example**:
  ```rust
  let config = GeneratorConfig {
      numa_mode: NumaMode::Force,
      ..Default::default()
  };
  ```

#### `NumaMode::Disabled`
- **Behavior**: Never use NUMA optimizations (even on multi-node systems)
- **Use case**: Cloud VMs, troubleshooting, baseline comparisons
- **Example**:
  ```rust
  let config = GeneratorConfig {
      numa_mode: NumaMode::Disabled,
      ..Default::default()
  };
  ```

### Python API

```python
import dgen_py

# Auto-detect (default)
data = dgen_py.generate_data(100 * 1024 * 1024, numa_mode="auto")

# Force NUMA optimizations
data = dgen_py.generate_data(100 * 1024 * 1024, numa_mode="force")

# Disable NUMA optimizations
data = dgen_py.generate_data(100 * 1024 * 1024, numa_mode="disabled")

# Streaming generator with NUMA control
gen = dgen_py.Generator(
    size=1024 * 1024 * 1024,
    dedup_ratio=2.0,
    compress_ratio=3.0,
    numa_mode="auto",
    max_threads=8
)
```

## Architecture Detection

The library automatically detects system architecture:

```python
import dgen_py

info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Total CPUs: {info['total_cpus']}")
    
    for node in info['nodes']:
        print(f"  Node {node['node_id']}: {node['num_cpus']} CPUs, {node['memory_gb']:.1f} GB")
else:
    print("NUMA feature not available")
```

Example output on 2-socket EPYC server:
```
NUMA nodes: 2
Total CPUs: 64
  Node 0: 32 CPUs, 128.0 GB
  Node 1: 32 CPUs, 128.0 GB
```

## Current Implementation Status

### ✅ Implemented
- NUMA topology detection (via /sys/devices/system/node)
- CPU count detection (num_cpus crate)
- Thread pool configuration (rayon ThreadPoolBuilder)
- Per-mode logging (INFO level shows NUMA status)

### 🔄 Planned (Future Enhancement)
- Thread pinning to specific NUMA nodes
- Memory allocation on local NUMA nodes
- Per-node memory pools
- Cross-node traffic minimization

**Note**: Current implementation uses rayon's default thread pool. NUMA optimizations are logged but thread pinning is not yet implemented.

## Performance Tuning Tips

### Cloud/VM Environments
```python
# Cloud VMs are typically UMA - disable NUMA for clarity
data = dgen_py.generate_data(size, numa_mode="disabled")
```

### Bare Metal Multi-Socket
```python
# Let auto-detection handle it
data = dgen_py.generate_data(size, numa_mode="auto")

# Or force if you know it's multi-socket
data = dgen_py.generate_data(size, numa_mode="force", max_threads=64)
```

### Memory-Bound Workloads
```python
# Reduce thread count to avoid memory bandwidth saturation
# Rule of thumb: 1 thread per 4-8 GB/s memory bandwidth
data = dgen_py.generate_data(size, max_threads=8)
```

### CPU-Bound Workloads
```python
# Use all cores for incompressible data (CPU-bound)
data = dgen_py.generate_data(size, compress_ratio=1.0)

# Reduce threads for compressible data (more work per byte)
data = dgen_py.generate_data(size, compress_ratio=5.0, max_threads=4)
```

## Tracing and Debugging

Enable INFO-level logging to see thread and NUMA configuration:

```bash
# Rust
RUST_LOG=info cargo run --example cpu_control

# Python
RUST_LOG=info python examples/demo.py
```

Example output:
```
INFO dgen_rs::generator: Using 12 threads for parallel generation
INFO dgen_rs::numa: Detected 1 NUMA node(s)
INFO dgen_rs::generator: NUMA optimization enabled: 1 nodes detected
```

## Example: Performance Comparison

```python
import dgen_py
import time

size = 1024 * 1024 * 1024  # 1 GiB

# Baseline: single-threaded
start = time.time()
data = dgen_py.generate_data(size, max_threads=1)
baseline_time = time.time() - start
baseline_throughput = size / baseline_time / 1e9
print(f"Single-thread: {baseline_throughput:.2f} GB/s")

# Multi-threaded: 8 cores
start = time.time()
data = dgen_py.generate_data(size, max_threads=8)
multi_time = time.time() - start
multi_throughput = size / multi_time / 1e9
speedup = baseline_time / multi_time
print(f"8 threads: {multi_throughput:.2f} GB/s ({speedup:.1f}x speedup)")

# All cores with NUMA
start = time.time()
data = dgen_py.generate_data(size, numa_mode="force")
numa_time = time.time() - start
numa_throughput = size / numa_time / 1e9
speedup = baseline_time / numa_time
print(f"All cores + NUMA: {numa_throughput:.2f} GB/s ({speedup:.1f}x speedup)")
```

## See Also

- [ARCHITECTURE.md](../docs/ARCHITECTURE.md) - Detailed architecture documentation
- [DEVELOPMENT.md](../docs/DEVELOPMENT.md) - Debugging and profiling guide
- [examples/cpu_control.rs](../examples/cpu_control.rs) - Rust examples
- [python/examples/demo.py](../python/examples/demo.py) - Python examples
