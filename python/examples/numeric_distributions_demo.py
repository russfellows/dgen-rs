#!/usr/bin/env python3
"""
numeric_distributions_demo.py

Demonstrates dgen-py v0.3.0's numeric distribution functions:
  - generate_uniform(count, low, high, ...)      -- parallel uniform float32
  - normalize_rows(buffer, dim, ...)              -- in-place L2 row normalize
  - generate_uniform_vectors(rows, dim, ...)      -- fused generate+normalize

Motivation: dgen-py's existing API (generate_buffer/Generator.get_chunk)
returns raw random bytes, not floats with a defined distribution. These
functions fill that gap for ML / vector-database style workloads (e.g.
generating embeddings for an ANN index benchmark) where you need actual
float32 values with a bounded range, optionally L2-normalized -- while
keeping dgen-py's real advantage: parallel, Rust-side generation instead
of a single-threaded NumPy bottleneck.

See docs/DESIGN_NUMERIC_DISTRIBUTIONS.md for the full design writeup and
README.md's "Pattern 4 — Numeric Distributions" section for the measured
14.7x-27.8x end-to-end speedup vs NumPy at real vector-database scale.
"""

import time

import numpy as np

import dgen_py


def banner(title: str) -> None:
    print(f"\n{'=' * 70}")
    print(f"  {title}")
    print(f"{'=' * 70}")


# ---------------------------------------------------------------------------
# 1. generate_uniform — parallel uniform float32 generation
# ---------------------------------------------------------------------------
banner("1. generate_uniform() — uniform float32 in [0, 1)")

view = dgen_py.generate_uniform(1_000_000)
arr = np.frombuffer(view, dtype=np.float32)
print(f"  count       : {arr.shape[0]:,}")
print(f"  all finite  : {np.isfinite(arr).all()}")
print(f"  range       : [{arr.min():.6f}, {arr.max():.6f})")

# Custom range
view2 = dgen_py.generate_uniform(100_000, low=-5.0, high=5.0)
arr2 = np.frombuffer(view2, dtype=np.float32)
print(f"  custom range: [{arr2.min():.3f}, {arr2.max():.3f})  (requested [-5, 5))")

# Deterministic with a seed
a = np.frombuffer(dgen_py.generate_uniform(1000, seed=42), dtype=np.float32)
b = np.frombuffer(dgen_py.generate_uniform(1000, seed=42), dtype=np.float32)
print(f"  seed=42 reproducible: {np.array_equal(a, b)}")

# ---------------------------------------------------------------------------
# 2. normalize_rows — in-place L2 row normalization
# ---------------------------------------------------------------------------
banner("2. normalize_rows() — in-place L2 normalization")

rows, dim = 10_000, 128
buf = bytearray(dgen_py.generate_uniform(rows * dim))
dgen_py.normalize_rows(buf, dim)
mat = np.frombuffer(buf, dtype=np.float32).reshape(rows, dim)
norms = np.linalg.norm(mat, axis=1)
print(f"  {rows:,} rows x {dim} dims")
print(f"  norm range  : [{norms.min():.6f}, {norms.max():.6f}]  (expect ~1.0)")

# Also works directly on a NumPy float32 array (typed buffer path)
np_arr = np.random.random((1000, 64)).astype(np.float32)
dgen_py.normalize_rows(np_arr.reshape(-1), dim=64)  # flatten view, in place
print(f"  works on NumPy float32 arrays too: "
      f"norms ~1.0 = {np.allclose(np.linalg.norm(np_arr, axis=1), 1.0, atol=1e-3)}")

# ---------------------------------------------------------------------------
# 3. generate_uniform_vectors — fused generate + normalize
# ---------------------------------------------------------------------------
banner("3. generate_uniform_vectors() — fused, the fast path")

rows, dim = 100_000, 512
t0 = time.perf_counter()
view = dgen_py.generate_uniform_vectors(rows, dim)
dgen_elapsed = time.perf_counter() - t0
vectors = np.frombuffer(view, dtype=np.float32).reshape(rows, dim)
print(f"  {rows:,} x {dim} vectors in {dgen_elapsed * 1000:.1f} ms")
print(f"  all finite, unit-normalized: "
      f"{np.isfinite(vectors).all() and np.allclose(np.linalg.norm(vectors, axis=1), 1.0, atol=1e-3)}")

# normalize=False for raw (unnormalized) vectors
view_raw = dgen_py.generate_uniform_vectors(1000, 32, normalize=False)
raw = np.frombuffer(view_raw, dtype=np.float32).reshape(1000, 32)
print(f"  normalize=False -> norms NOT ~1.0: "
      f"{not np.allclose(np.linalg.norm(raw, axis=1), 1.0, atol=1e-3)}")

# ---------------------------------------------------------------------------
# 4. Head-to-head vs NumPy (the contract this replaces)
# ---------------------------------------------------------------------------
banner("4. dgen_py.generate_uniform_vectors() vs NumPy generate_vectors()")


def numpy_generate_vectors(n, d):
    v = np.random.random((n, d)).astype(np.float32)
    v /= np.linalg.norm(v, axis=1, keepdims=True)
    return v


for rows, dim in [(10_000, 512), (100_000, 512), (1_000_000, 512)]:
    t0 = time.perf_counter()
    numpy_generate_vectors(rows, dim)
    numpy_ms = (time.perf_counter() - t0) * 1000

    t0 = time.perf_counter()
    dgen_py.generate_uniform_vectors(rows, dim)
    dgen_ms = (time.perf_counter() - t0) * 1000

    print(f"  {rows:>9,} x {dim:<4}  numpy={numpy_ms:8.1f}ms  "
          f"dgen_py={dgen_ms:7.1f}ms  speedup={numpy_ms / dgen_ms:5.1f}x")

print("\nDone. See README.md 'Pattern 4 — Numeric Distributions' for full")
print("production-scale (1M x 1536) benchmark results.")
