# Changelog

All notable changes to dgen-rs/dgen-py will be documented in this file.

## [Unreleased]

### Added
- Initial implementation of high-performance data generation
- Xoshiro256++ RNG for fast keystream generation
- Controllable deduplication ratios (1:1 to N:1)
- Controllable compression ratios (1:1 to N:1)
- NUMA topology detection via /sys filesystem (Linux)
- NUMA-aware parallel generation (optional)
- Zero-copy Python bindings via PyO3
- Simple API: `generate_data()` / `generate_buffer()`
- Zero-copy API: `fill_buffer()` / `generate_into_buffer()`
- Streaming API: `Generator` class
- Python buffer protocol support (bytearray, memoryview, numpy)
- Comprehensive documentation and examples

### Performance
- 5-15 GB/s per core (incompressible data)
- 1-4 GB/s per core (compressible data)
- Near-linear multi-core scaling with rayon

### Credits
- Algorithm ported from s3dlio/src/data_gen_alt.rs
- NUMA detection from kv-cache-bench
- Built with PyO3 and Maturin

## [0.1.0] - 2026-01-08

Initial release.
