# dgen-py

**The world's fastest Python random data generator — NUMA-aware, zero-copy, with configurable deduplication and compression ratios**

[![Version](https://img.shields.io/badge/version-0.3.0-blue)](https://pypi.org/project/dgen-py/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![PyPI](https://img.shields.io/pypi/v/dgen-py)](https://pypi.org/project/dgen-py/)
[![Python Version](https://img.shields.io/badge/python-3.11+-blue.svg)](https://www.python.org)
[![Tests](https://img.shields.io/badge/tests-63%20passing-success)](https://github.com/russfellows/dgen-rs)

## Features

- **Numeric Distributions** (v0.3.0): `generate_uniform()`, `normalize_rows()`, `generate_uniform_vectors()` — parallel uniform float32 generation and L2 row normalization in Rust, up to **28× faster end-to-end** than NumPy for ML/vector-database workloads (embeddings, ANN indexes)
- **Global Thread Pool** (v0.2.4): process-global `OnceLock<ThreadPool>` shared across all callers; auto-sizes via sibling-process detection so concurrent callers never spawn too many OS threads
- **Zero-Copy Slices** (v0.2.3) Using a Rolling Pool / BufferPool: zero-copy slices from a pre-generated 1 MB block — 16× speedup for 64 KB objects; `generate_buffer()` uses it automatically
- **Bulk Memory Allocation** (v0.2.0): `create_bytearrays()` is 1,280× faster than Python list comprehension for pre-generating large buffer sets
- **Dynamic Reseeding** (v0.1.7): `set_seed()` resets or alternates data streams without recreating `Generator`
- **Reproducible Seeds** (v0.1.6): `seed=` parameter for deterministic, verifiable generation across runs
- **Controllable Data**: configurable `dedup_ratio` and `compress_ratio` — unique among Python data generators; essential for realistic storage workload simulation
- **Streaming API**: terabytes of data with constant 32 MB memory footprint
- **Zero-Copy**: Python buffer protocol — direct memory access, no data copying between Rust and Python
- **NUMA-Aware**: (Only with locally built wheels) Numa one-process-per-node with numa local memory and CPU allocation and core affinity
- **Built with Rust**: Xoshiro256++ RNG, Rayon thread-parallel generation, PyO3 bindings

---

## Why dgen-py? Because it is literally up to 200X faster than NumPy

NumPy, the most common Python data generator tops out around 2 GB/s with multiple cores. dgen-py generates at **memory-bus speed** using all cores in parallel. Tested exhaustively — all 5 NumPy bit-generators (MT19937, PCG64, PCG64DXSM, SFC64, Philox), single- and multi-threaded, with every trick a NumPy power-user would try:

**System:** Intel Xeon Platinum 8280L, 28 vCPU — 100 GB test

| Method | Threads | Throughput | vs Baseline | Memory Required |
|--------|:-------:|:----------:|:-----------:|:---------------:|
| `os.urandom()` (baseline) | 1 | 0.34 GB/s | 1× | minimal |
| NumPy best-case (SFC64, 28 threads)† | 28 | ~1.4 GB/s | 4× | 100 GB RAM |
| **dgen-py streaming (32 MB chunks)** | **28** | **69.09 GB/s** | **203×** | **32 MB RAM** |

† *Tried all 5 bit-generators and every multi-threading strategy. All single-threaded generators land at 0.45–0.57 GB/s — switching generator makes no difference. Threading tops out at ~1.4 GB/s because `rng.bytes()` always allocates a new Python object and the GIL serializes those allocations. `integers(out=...)` doesn't exist. There is no way to make NumPy faster for this task.*

- **50× faster than the best NumPy**, **203× faster than `os.urandom`** on a 24 vCPU VM - Up to 200x on 128+ CPU cores
- **3,000× less memory** at scale (32 MB working set vs. 100 GB for a 100 GB dataset)
- **Dgen-Py achieves up to 300 GB/s** on large Gen5 CPU systems with 64 cores or more
- **Only dgen-py** supports `dedup_ratio` and `compress_ratio` — `os.urandom` and NumPy always produce max-entropy data, making it unsuitable for realistic storage workload testing

---

## Installation

### From PyPI (Recommended)

```bash
pip install dgen-py
```

Requires Python 3.11+. PyPI wheels are built without NUMA/hwloc so they run on all Linux distributions. For UMA and single-node cloud systems, performance is unaffected.

### Enable NUMA Support (Source Build)

```bash
# Ubuntu/Debian
sudo apt-get install libudev-dev libhwloc-dev

# RHEL/CentOS/Fedora
sudo yum install systemd-devel hwloc-devel

pip install --no-binary dgen-py dgen-py \
    --config-settings=build-args="--features python-bindings,numa,thread-pinning"
```

---

## Quick Start

Five patterns cover the most common use cases:

```python
import dgen_py
import numpy as np

# ── 1. Streaming ────────────────────────────────────────────────────────────
# Generates any amount of data with constant 32 MB memory, all cores used.
gen = dgen_py.Generator(size=100 * 1024**3)   # 100 GB total
buf = bytearray(gen.chunk_size)                # allocate one 32 MB reusable buffer

while not gen.is_complete():
    n = gen.fill_chunk(buf)                    # fills buf in-place, returns bytes written
    if n == 0:
        break
    output.write(memoryview(buf)[:n])          # or upload to S3, send over network, etc.

# ── 2. Small slices (< 1 MB objects) ────────────────────────────────────────
# BufferPool generates one 1 MB block and serves zero-copy slices from it.
# No re-generation until the block is exhausted; ideal for tight per-object loops.
pool = dgen_py.BufferPool(dedup_ratio=1.0, compress_ratio=1.0)

for _ in range(10_000):
    obj = pool.next_slice(64 * 1024)           # 64 KB zero-copy BytesView; pool auto-refills
    storage_client.put(obj)

# For one-off slices, generate_buffer() uses the same pool automatically:
buf = dgen_py.generate_buffer(256 * 1024)      # 256 KB, zero-copy, no setup needed

# ── 3. Bulk buffer allocation ────────────────────────────────────────────────
# create_bytearrays() is ~1,280× faster than a Python list comprehension.
# Python: [bytearray(32*1024**2) for _ in range(768)] takes seconds or more.
# dgen-py: direct C API (PyByteArray_Resize) from Rust — completes in milliseconds.
n_chunks = 768
chunk_sz  = 32 * 1024**2                       # 32 MB each → 24 GB total

buffers = dgen_py.create_bytearrays(count=n_chunks, size=chunk_sz)

gen = dgen_py.Generator(size=n_chunks * chunk_sz)
for buf in buffers:
    gen.fill_chunk(buf)                        # fill all buffers at full parallel speed

# ── 4. Seed and reset ────────────────────────────────────────────────────────
# seed= makes output reproducible across runs.
# set_seed() resets or alternates streams without recreating the Generator.
gen = dgen_py.Generator(size=100 * 1024**3, seed=42)
buf = bytearray(gen.chunk_size)

gen.fill_chunk(buf)        # stream A — deterministic, same every run with seed=42
gen.set_seed(99)           # switch to a different stream
gen.fill_chunk(buf)        # stream B
gen.set_seed(42)           # rewind back to the beginning of stream A
gen.fill_chunk(buf)        # identical bytes to the very first fill_chunk above

# ── 5. Numeric distributions (ML / vector-database data) ───────────────────
# Actual float32 values with a defined distribution, not raw bytes.
# Up to 28x faster end-to-end than NumPy — see Pattern 4 below for details.
view = dgen_py.generate_uniform_vectors(rows=1_000_000, dim=1536)
vectors = np.frombuffer(view, dtype=np.float32).reshape(1_000_000, 1536)
```

See [API Usage](#api-usage) below for detailed options on each pattern.

---

## API Usage

Three patterns cover the main scaling scenarios. Choose based on object size and concurrency.

### Pattern 1 — Streaming: one process, all cores (`Generator + fill_chunk`)

One `Generator` per process with `max_threads=None`. The global Rayon pool parallelises
each `fill_chunk()` call across all cores using 1 MiB Xoshiro256++ blocks.
Best for large objects and single-process bulk generation.

```python
gen = dgen_py.Generator(
    size=100 * 1024**3,      # total bytes to generate
    dedup_ratio=1.0,         # 1.0 = no dedup, 2.0 = 2:1, i.e. 50% of data is duplicate, etc.
    compress_ratio=1.0,      # 1.0 = incompressible, 2.0 = 2:1 compressible
    numa_mode="auto",        # auto-detect topology
    max_threads=None,        # use all available cores
    # chunk_size=64*1024**2  # optional: override default 32 MB
)
buf = bytearray(gen.chunk_size)
while not gen.is_complete():
    n = gen.fill_chunk(buf)
    if n == 0:
        break
    output.write(memoryview(buf)[:n])
```

**Streaming throughput by chunk size (28-core Xeon):**

| Chunk size | Throughput |
|:---:|:---:|
| 32 MB (default) | ~68 GB/s |
| 64 MB | ~73 GB/s |

> 64 MB chunks give ~7% higher throughput on newer CPUs (Sapphire/Emerald Rapid) with large L3 caches.

### Pattern 2 — Concurrent: N processes, 1 thread each (`max_threads=1`)

Each process creates its own `Generator(max_threads=1)` — pure sequential Xoshiro256++
per process, no inter-process pool contention. Scales linearly because each process runs
an independent RNG. Ideal for DLIO DataLoader workers and multi-threaded server benchmarks.

```python
# Each Python process (e.g. DLIO DataLoader worker):
gen = dgen_py.Generator(size=256 * 1024**3, max_threads=1)
buf = bytearray(object_size)
while generating:
    gen.fill_chunk(buf)
    output.write(buf)
```

**Aggregate throughput (28-core Xeon, 8 MiB objects, 5 s):**

| N processes | Aggregate | Per-process |
|:-----------:|:---------:|:-----------:|
| 1  | 5.2 GB/s  | 5.2 GB/s |
| 4  | 17.8 GB/s | 4.5 GB/s |
| 8  | 27.9 GB/s | 3.5 GB/s |
| 16 | 52.7 GB/s | 3.3 GB/s |
| 28 | **58.6 GB/s** | 2.1 GB/s |

Aggregate throughput saturated VM DRAM bandwidth (~58 GB/s) at 28 processes.

### Pattern 3 — Small Objects < 1 MB: `BufferPool`

For high-frequency small-object workloads (JPEG/PNG images, NPZ shards, etc.),
`BufferPool` generates one 1 MB backing block and serves zero-copy slices with no
re-generation overhead until exhausted. At 315 KB this saves ~70% of generation work
versus a fresh `Generator` per call.

`generate_buffer(size)` uses a thread-local pool automatically for `size < 1 MB` —
no code change needed for existing single-threaded callers.

```python
# Automatic (single-threaded, no changes needed)
buf = dgen_py.generate_buffer(64 * 1024)   # BytesView, zero-copy, from pool

# Explicit pool for tight loops, multiple helpers, or custom config
pool = dgen_py.BufferPool(dedup_ratio=1.0, compress_ratio=1.0)

for _ in range(10_000):
    obj = pool.next_slice(64 * 1024)       # zero-copy; refills automatically
    storage_client.put(obj)

# Change characteristics mid-stream (forces one pool refill, then continues zero-copy)
pool.reconfigure(compress_ratio=4.0)

print(pool.remaining)       # bytes left before next refill (0..1_048_576)
print(pool.compress_ratio)  # current compress factor
```

**When to use `BufferPool` vs `generate_buffer()`:**

| Scenario | Best choice |
|----------|-------------|
| Existing code, objects < 1 MB | `generate_buffer()` — automatic, no code change |
| New code, tight loop, many small objects | `BufferPool` — explicit, identical performance |
| Objects ≥ 1 MB | Either — both use the same large-object bypass path |
| NUMA-pinned workloads | `generate_buffer(..., numa_node=N)` — pool bypassed per design |

### Pattern 4 — Numeric Distributions: ML / vector-database data (v0.3.0)

For workloads that need actual **floating-point values with a defined
distribution** — not raw bytes — rather than NumPy. Built for cases like
vector-database benchmarks (embeddings) and other ML data-generation
paths where the existing byte-oriented API (`generate_buffer`,
`Generator`) isn't a valid substitute: naively reinterpreting random
bytes as `float32` produces NaN/Inf and unbounded values.

```python
import dgen_py
import numpy as np

# ── Uniform float32 generation ──────────────────────────────────────────────
# count is an ELEMENT count (not bytes) — deliberately distinct from
# generate_buffer's byte-count `size`.
view = dgen_py.generate_uniform(1_000_000, low=0.0, high=1.0)
arr = np.frombuffer(view, dtype=np.float32)          # zero-copy, flat array

# ── L2-normalize an existing float32 buffer, in place ───────────────────────
buf = bytearray(dgen_py.generate_uniform(1_000 * 128))
dgen_py.normalize_rows(buf, dim=128)                  # every row -> unit L2 norm
mat = np.frombuffer(buf, dtype=np.float32).reshape(1_000, 128)

# ── Fused generate + normalize — the fast path for vector data ─────────────
# One Rust call: no Python round-trip between generation and normalization.
view = dgen_py.generate_uniform_vectors(rows=1_000_000, dim=1536)
vectors = np.frombuffer(view, dtype=np.float32).reshape(1_000_000, 1536)
```

**Why this exists:** an initial pure-Python workaround (bit-mask raw
bytes into valid floats, then `np.linalg.norm` + divide) proved dgen-py's
generation engine is ~80× faster than NumPy at raw generation — but the
single-threaded Python-side conversion erased nearly all of that
advantage. Moving the bit-mask conversion and L2-normalization into Rust
(reusing the same `par_chunks_mut` engine as the rest of dgen-py) restores
real multi-core scaling to the whole pipeline, not just the raw-byte step.

**Real-world speedup vs. NumPy** (`np.random.random((n,dim)).astype(np.float32)`
+ `np.linalg.norm` + divide — the exact contract this replaces), measured
against a real vector-database benchmark's production configurations,
28-core Xeon:

| Shape (rows × dim) | NumPy | `generate_uniform_vectors` | Speedup |
|:---:|:---:|:---:|:---:|
| 10,000 × 512      | 65.7 ms   | 4.5 ms   | **14.7×** |
| 10,000 × 1,536    | 207.1 ms  | 11.1 ms  | **18.7×** |
| 100,000 × 512     | 712.6 ms  | 35.0 ms  | **20.4×** |
| 100,000 × 1,536   | 2147.0 ms | 98.8 ms  | **21.7×** |
| 1,000,000 × 512   | 7133.5 ms | 281.1 ms | **25.4×** |
| 1,000,000 × 1,536 | 20986.9 ms| 755.3 ms | **27.8×** |

Speedup grows with scale — the two largest, most production-representative
shapes hit 25–28×. `generate_uniform_vectors` also has genuine per-call
thread-count control via `max_threads=`, unlike the streaming `Generator`
API's coarser sequential-vs-parallel gate:

| Threads | Throughput (1M × 1536 vectors) |
|:-------:|:-------------------------------:|
| 1  | 0.69 GB/s |
| 2  | 1.32 GB/s |
| 4  | 2.46 GB/s |
| 8  | 4.47 GB/s |
| 16 | 6.56 GB/s |
| 28 | 7.63 GB/s |

**Design notes:**
- All three functions return `BytesView` (never a NumPy array) and use a
  flat namespace — consistent with the rest of dgen-py rather than
  adopting NumPy's API shape. See
  [`docs/DESIGN_NUMERIC_DISTRIBUTIONS.md`](docs/DESIGN_NUMERIC_DISTRIBUTIONS.md)
  for the full design writeup and rationale.
- `normalize_rows()` is L2-only in v1, accepts any writable, C-contiguous
  `float32` NumPy array or raw-byte buffer (`bytearray`, `memoryview`,
  dgen-py's own `BytesView`) — other typed buffers (e.g. `float64`) are
  rejected with `ValueError`, never silently byte-reinterpreted. A
  zero-norm row is left unchanged rather than turned into NaN.
- `generate_uniform_vectors(..., normalize=False)` skips normalization
  entirely when you just want raw uniform-distributed vectors.

### Bulk Pre-Allocation: `create_bytearrays` (v0.2.0)

When you need all data in RAM before writing (DLIO benchmark, batch loaders):

```python
total   = 24 * 1024**3   # 24 GB
chunk   = 32 * 1024**2   # 32 MB per chunk
n       = total // chunk  # 768 chunks

# 1,280× faster than [bytearray(chunk) for _ in range(n)]
buffers = dgen_py.create_bytearrays(count=n, size=chunk)   # ~10 ms

gen = dgen_py.Generator(size=total, max_threads=None)
for buf in buffers:
    gen.fill_chunk(buf)

for buf in buffers:
    f.write(buf)
```

Uses Python C API (`PyByteArray_Resize`) directly from Rust; for 32 MB chunks glibc
uses `mmap` (≥128 KB threshold) for zero-copy page allocation.

### Reproducible Data: `seed` and `set_seed` (v0.1.6 / v0.1.7)

```python
# Same seed → identical bytes every run
gen1 = dgen_py.Generator(size=10 * 1024**3, seed=12345)
gen2 = dgen_py.Generator(size=10 * 1024**3, seed=12345)
# gen1 and gen2 produce IDENTICAL streams

# No seed → different data each run (default)
gen3 = dgen_py.Generator(size=10 * 1024**3)

# set_seed() resets or alternates streams without recreating Generator
gen = dgen_py.Generator(size=100 * 1024**3, seed=1111)
buf = bytearray(10 * 1024**2)

gen.set_seed(1111); gen.fill_chunk(buf)  # Pattern A
gen.set_seed(2222); gen.fill_chunk(buf)  # Pattern B
gen.set_seed(1111); gen.fill_chunk(buf)  # SAME as first chunk — Pattern A
```

**Use cases:** RAID stripe testing with alternating patterns, multi-phase AI/ML workloads,
reproducible CI/CD benchmarks, stream reset without Generator recreation.

### Data Characteristics: `dedup_ratio` and `compress_ratio`

Choose based on the workload you are testing — not performance:

| `compress_ratio` | Data character | Typical use |
|:---:|---|---|
| 1.0 | Incompressible (encrypted, archives) | Test compression engines accurately |
| 2.0 | 2:1 compressible (text, logs) | Realistic mixed workloads |
| 4.0+ | Highly compressible | Dedup/compress stress tests |

`compress_ratio=2.0` generates data ~30–50% faster (more zero bytes) but inflates
storage efficiency metrics if compression is enabled on the target system.
`dedup_ratio` has < 1% performance variance.

### Concurrent Callers and Global Pool (v0.2.4)

Before v0.2.4, each `DataGenerator::new()` created a fresh Rayon `ThreadPool`.
At c=28 concurrent callers this spawned 784 OS threads on 28 cores, causing a
throughput cliff. The fix is a process-global `OnceLock<Arc<ThreadPool>>` shared
by all callers.

**Auto-sizing priority:**
1. `DGEN_THREADS` env var
2. `RAYON_NUM_THREADS` env var
3. `cpu_affinity ÷ live_sibling_dgen_processes` — PID files in `/tmp/dgen-<uid>/`
4. Total logical CPUs (fallback)

**Concurrency benchmark (28-core Xeon, 8 MiB objects, 5 s):**

| Concurrency | Before (new pool per call) | After (global pool) |
|:-----------:|:--------------------------:|:-------------------:|
| c=1  | ~5 GB/s  | ~5 GB/s |
| c=16 | ~52 GB/s | ~52 GB/s |
| c=28 | ~45 GB/s (contention) | **~58 GB/s** (+29%) |

### `thread_local` Module (v0.2.4, Rust API)

Canonical zero-copy API for async HTTP servers — eliminates `thread_local! { RefCell<RollingPool> }` boilerplate:

```rust
use dgen_data::thread_local::next_slice;

fn get_chunk(chunk_size: usize) -> bytes::Bytes {
    next_slice(chunk_size)   // zero-copy Arc slice, no re-seeding, no locking
}
```

### System Information

```python
info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")
```

### Multi-Process NUMA

For maximum throughput on multi-socket systems, run **one Python process per NUMA node**
pinned to local cores via `os.sched_setaffinity()`.

```python
gen = dgen_py.Generator(size=..., numa_mode="auto", numa_node=0)  # bind to node 0
```

See [`python/examples/benchmark_numa_multiprocess_v2.py`](python/examples/benchmark_numa_multiprocess_v2.py)
for the complete multi-process implementation.

---

## Performance

### Streaming Comparison vs Common Alternatives

See [Why dgen-py?](#why-dgen-py-because-it-destroys-numpy) above for the head-to-head NumPy comparison with full methodology.

### Per-Object and Streaming — 28-core Xeon (v0.2.4)

Run `cargo run --release --example speed-table` (Rust) or
`python python/examples/bench_generation_speeds.py` (Python) to reproduce on your hardware.

**Per-object:** tight loop, generation time only.
- dgen-py ≤ 1 MB: `BufferPool.next_slice()` — zero-copy from pre-generated pool
- dgen-py > 1 MB: `generate_data_simple()` — Rayon parallel per call
- NumPy: `np.random.default_rng().integers(...)` — PCG64, single-threaded

**Streaming (dgen-py):** one `Generator` for the full run, 32 MB chunks, pool reused.

| Object | Rust per-obj | dgen-py per-obj | NumPy per-obj | dgen-py stream |
|--------|:---:|:---:|:---:|:---:|
| 64 B   | 1.60 GB/s | 219 MB/s  | 9 MB/s    | ~67 GB/s |
| 512 B  | 4.63 GB/s | 1.43 GB/s | 69 MB/s   | ~67 GB/s |
| 4 KB   | 6.06 GB/s | 4.38 GB/s | 371 MB/s  | ~67 GB/s |
| 64 KB  | 6.31 GB/s | 6.15 GB/s | 929 MB/s  | ~67 GB/s |
| 1 MB   | 6.33 GB/s | 6.29 GB/s | 1.01 GB/s | ~67 GB/s |
| 10 MB  | 9.47 GB/s | 6.75 GB/s | 948 MB/s  | ~67 GB/s |
| 100 MB | 19.31 GB/s | 6.39 GB/s | 886 MB/s | ~74 GB/s |
| 1 GB   | 24.83 GB/s | 16.15 GB/s | 891 MB/s | ~74 GB/s |
| 10 GB  | **28.55 GB/s** | **18.29 GB/s** | 892 MB/s | **~74 GB/s** |

**Key observations:**

- **NumPy plateaus at ~900 MB/s** for objects ≥ 1 MB — PCG64 is single-threaded and saturates one core's DRAM write bandwidth; it has no streaming API and requires the full dataset in RAM.
- **dgen-py and Rust scale to 18–29 GB/s** at 100 MB+ via all-core Rayon Xoshiro256++; at 10 GB they approach peak DRAM write bandwidth (~29 GB/s single-socket).
- **Streaming hits 67–74 GB/s** by reusing the Rayon pool across 32 MB chunks; the working set stays in L3 and only the final writeback reaches DRAM.
- **Only dgen-py** supports configurable `dedup_ratio` and `compress_ratio`. Other generators (`os.urandom`, NumPy, Numba) produce max-entropy data unsuitable for realistic storage workload testing.

### Small-Object Pool Improvement — v0.2.3 (per-object, 1 GB total output)

| Object size | v0.2.2 (`generate_data_simple`) | v0.2.3 `RollingPool` | Speedup |
|:---:|:---:|:---:|:---:|
| 64 KB | 107 MB/s  | 1.73 GB/s | **16×** |
| 1 MB  | 1.78 GB/s | 1.74 GB/s | ≈1× |
| 1 GB  | 9.73 GB/s | 9.49 GB/s | ≈1× |

Zero regression for objects ≥ 1 MB; large-object bypass path is unaffected.

### Multi-NUMA Scalability — GCP Emerald Rapid

1,024 GB workload, `compress_ratio=1.0`:

| Instance | Cores | NUMA Nodes | Aggregate | Per-Core | Scaling |
|----------|:---:|:---:|:---:|:---:|:---:|
| C4-8  | 4  | 1 | 36.26 GB/s  | 9.07 GB/s  | baseline |
| C4-16 | 8  | 1 | **86.41 GB/s**  | **10.80 GB/s** | **119%** |
| C4-32 | 16 | 1 | **162.78 GB/s** | **10.17 GB/s** | **112%** |
| C4-96 | 48 | 2 | 248.53 GB/s | 5.18 GB/s  | 51%* |

\* *NUMA penalty on multi-socket: 49% per-core reduction, but highest absolute throughput*

- Excellent UMA scaling: 112–119% efficiency (super-linear due to larger aggregate L3 cache)
- `compress_ratio=2.0` gives 1.3–1.5× generation speedup — choose based on workload realism, not speed

**See [docs/BENCHMARK_RESULTS_V0.1.5.md](docs/BENCHMARK_RESULTS_V0.1.5.md) for detailed analysis.**

---

## Python Example Files

| File | Category | Description |
|------|----------|-------------|
| [`Benchmark_dgen-py_FIXED.py`](python/examples/Benchmark_dgen-py_FIXED.py) | Benchmark | Streaming throughput: 32 MB vs 64 MB chunks, 100 GB × 3 runs |
| [`bench_generation_speeds.py`](python/examples/bench_generation_speeds.py) | Benchmark | Comprehensive: fill_chunk, generate_buffer, create_bytearrays, file streaming |
| [`test_small_object_v023.py`](python/examples/test_small_object_v023.py) | Test + Bench | BufferPool correctness checks + per-object speed vs NumPy |
| [`test_set_seed_method.py`](python/examples/test_set_seed_method.py) | Test | `set_seed()` correctness and stream alternation |
| [`test_seed_reproducibility.py`](python/examples/test_seed_reproducibility.py) | Test | `seed=` determinism across runs |
| [`benchmark_numa_multiprocess_v2.py`](python/examples/benchmark_numa_multiprocess_v2.py) | Multi-NUMA | One-process-per-node architecture with `os.sched_setaffinity()` |
| [`storage_benchmark.py`](python/examples/storage_benchmark.py) | Storage | End-to-end: generation → file / S3 write |
| [`numeric_distributions_demo.py`](python/examples/numeric_distributions_demo.py) | Demo + Bench | `generate_uniform`, `normalize_rows`, `generate_uniform_vectors` — usage + speedup vs NumPy |
| [`test_chunk_sizes.py`](python/examples/test_chunk_sizes.py) | Benchmark | `Generator` streaming throughput across chunk sizes (4-128 MB) |

---

## Architecture

### Core Design

- **1 MiB internal blocks** (`BLOCK_SIZE`): distributed across all cores via Rayon; tuned to L3 cache
- **Xoshiro256++ RNG**: 5–10× faster than ChaCha20; independent per-block seeds for correct dedup and compression ratios
- **Global `OnceLock<Arc<ThreadPool>>`**: created on first `DataGenerator::new()`, shared by all subsequent callers; no per-call pool overhead
- **Zero-copy Python buffer protocol**: GIL released during generation (true parallelism); memoryview creation < 0.001 ms

### NUMA Architecture

One Python process per NUMA node; each process allocates memory locally and pins to local cores.
The PyPI wheel omits hwloc for broad compatibility; source builds enable full topology detection
via [hwlocality](https://crates.io/crates/hwlocality).

---

## Use Cases

- **Storage benchmarking**: generate realistic dedup/compress workloads at 40–248 GB/s
- **AI/ML data loading**: simulate DLIO, NPZ, and checkpoint read pipelines
- **Network testing**: high-throughput data sources with constant memory footprint
- **Compression/dedup validation**: exact control over data compressibility and dedup ratio
- **Reproducible CI/CD**: identical workloads across test runs via `seed=`

---

## License

Dual-licensed under MIT OR Apache-2.0

## Credits

- Built with [PyO3](https://pyo3.rs/) and [Maturin](https://www.maturin.rs/)
- NUMA topology via [hwlocality](https://crates.io/crates/hwlocality)
- Xoshiro256++ from the [rand](https://crates.io/crates/rand) crate

