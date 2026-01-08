# Development Guide

## Debugging with Tracing

dgen-rs uses the `tracing` crate for structured logging. You can enable different log levels:

### Environment Variables

```bash
# Show all logs (TRACE level - very verbose)
RUST_LOG=trace cargo test

# Show debug logs (recommended for development)
RUST_LOG=debug cargo test

# Show info logs only (default for production)
RUST_LOG=info cargo test

# Filter by module
RUST_LOG=dgen_rs::generator=debug cargo test
```

### Log Levels

- **TRACE**: Extremely detailed, shows every block generation
- **DEBUG**: Detailed execution flow, allocation, parallel execution
- **INFO**: High-level operations (data generation start/complete)
- **WARN**: Warning messages (not currently used)
- **ERROR**: Error messages

### Example Output

```bash
$ RUST_LOG=debug cargo test test_generate_minimal -- --nocapture

2026-01-08T13:50:20.755941Z  INFO dgen_rs::generator: Starting data generation: size=100, dedup=1, compress=1
2026-01-08T13:50:20.756031Z DEBUG dgen_rs::generator: Generating: size=4194304, blocks=1, dedup=1, unique_blocks=1, compress=1
2026-01-08T13:50:20.756233Z DEBUG dgen_rs::generator: Allocating 4194304 bytes (1 blocks)
2026-01-08T13:50:20.756300Z DEBUG dgen_rs::generator: Starting parallel generation with rayon
2026-01-08T13:50:20.813882Z DEBUG dgen_rs::generator: Parallel generation complete, truncating to 4194304 bytes
```

## Performance Notes

### Streaming Generator Chunk Size

**CRITICAL**: The `DataGenerator::fill_chunk()` method generates full 4 MiB blocks internally.

- ✅ **Efficient**: Use chunk size >= 4 MiB (BLOCK_SIZE)
- ⚠️  **Inefficient**: Small chunks (1-8 KiB) cause massive overhead

Example:
```rust
// GOOD: 5 iterations for 20 MiB
let mut chunk = vec![0u8; 4 * 1024 * 1024]; // 4 MiB chunks

// BAD: 20,480 iterations for 20 MiB (4096x overhead!)
let mut chunk = vec![0u8; 1024]; // 1 KiB chunks
```

The inefficiency occurs because:
1. Each `fill_chunk()` call generates a full 4 MiB block
2. Only copies the requested amount (e.g., 1024 bytes)
3. Discards the rest

**Future Optimization**: Add block caching or use on-demand generation

### Recommended Chunk Sizes

| Use Case | Chunk Size | Notes |
|----------|-----------|-------|
| **Streaming to disk** | 4-16 MiB | Matches block size, optimal |
| **Network upload** | 4-8 MiB | Balance memory/efficiency |
| **In-memory processing** | Use `generate_data()` instead | Much faster |
| **Small writes** | Avoid streaming | Use simple API |

## Testing

### Run All Tests

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_streaming_generator

# Run with debug logging
RUST_LOG=debug cargo test -- --nocapture
```

### Test Coverage

Current tests:
1. `test_generate_minimal` - Minimum size generation
2. `test_generate_exact_block` - Exact block size
3. `test_generate_multiple_blocks` - Multiple blocks
4. `test_streaming_generator` - Streaming API
5. `test_detect_topology` - NUMA detection (Linux only)

## Building

### Rust Library

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# With specific features
cargo build --release --features numa
cargo build --release --no-default-features --features python-bindings
```

### Python Extension

```bash
# Development build (installs in current venv)
./build_pyo3.sh

# Production wheel
maturin build --release

# Install wheel
./install_wheel.sh
```

## Profiling

### With perf (Linux)

```bash
# Record
cargo build --release
perf record --call-graph=dwarf ./target/release/examples/benchmark

# Analyze
perf report
```

### With flamegraph

```bash
cargo install flamegraph
cargo flamegraph --bin benchmark
```

## Common Issues

### Issue: Tests hang forever

**Symptom**: Test runs for minutes without completing

**Cause**: Using small chunk sizes with streaming generator

**Fix**: Use chunk size >= 4 MiB (BLOCK_SIZE)

```rust
// Before (hangs)
let mut chunk = vec![0u8; 1024];

// After (fast)
let mut chunk = vec![0u8; 4 * 1024 * 1024];
```

### Issue: NUMA detection fails

**Symptom**: `NUMA info not available`

**Cause**: 
- Running in container without /sys mounted
- Non-Linux platform
- Cloud VM without NUMA topology

**Fix**: This is expected on UMA systems, no action needed

### Issue: Python import fails

**Symptom**: `ImportError: cannot import name '_dgen_rs'`

**Cause**: Python extension not built or installed

**Fix**:
```bash
cd dgen-rs
maturin develop --release
```

## Code Organization

```
dgen-rs/
├── src/
│   ├── lib.rs              # Module orchestration, PyO3 entry point
│   ├── generator.rs        # Core data generation logic
│   ├── constants.rs        # Constants (BLOCK_SIZE, etc.)
│   ├── numa.rs            # NUMA topology detection (Linux)
│   └── python_api.rs      # Zero-copy PyO3 bindings
├── python/
│   ├── dgen_py/
│   │   ├── __init__.py    # Python package with convenience wrappers
│   │   └── __init__.pyi   # Type stubs
│   ├── tests/
│   │   └── test_basic.py  # Python tests
│   └── examples/
│       └── demo.py        # Usage examples
└── tests/                 # Rust integration tests
```

## Future Improvements

1. **Block Caching**: Cache generated blocks in streaming mode
2. **On-Demand Generation**: Generate only requested bytes
3. **SIMD Optimization**: Use SIMD instructions for memcpy
4. **NUMA Thread Pinning**: Pin rayon threads to NUMA nodes
5. **Async API**: Async/await interface for Python
