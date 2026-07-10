#!/usr/bin/env python3
"""
tests/test_numeric_distributions_py.py

Correctness tests for the numeric-distribution primitives added for
mlcommons/storage#625 / docs/DESIGN_NUMERIC_DISTRIBUTIONS.md:
  - generate_uniform(count, low, high, ...) -> BytesView
  - normalize_rows(buffer, dim, ...) -> None (in-place, L2 only)
  - generate_uniform_vectors(rows, dim, ..., normalize=True) -> BytesView

RED-then-GREEN: this file is written BEFORE the PyO3 wrappers exist, so
running it against the currently-built extension must fail with
AttributeError (dgen_py has no such function yet). That AttributeError is
the RED signal, confirmed before any implementation lands.

Mirrors tests/test_dgen_py_v024.py's script style (banner() + linear
asserts), matching this repo's existing Python test convention.
"""

import numpy as np

# dgen-py is installed from the dgen-rs workspace via maturin develop
import dgen_py as dgen


def banner(title: str) -> None:
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}")


def as_numpy(view, count, dtype=np.float32):
    return np.frombuffer(memoryview(view), dtype=dtype, count=count)


# ---------------------------------------------------------------------------
# 1. generate_uniform — finite, in-range, count semantics
# ---------------------------------------------------------------------------
banner("1. generate_uniform() — finite values in [0, 1)")

COUNT = 100_000
view = dgen.generate_uniform(COUNT)
arr = as_numpy(view, COUNT)
assert arr.shape == (COUNT,), f"expected {COUNT} elements, got {arr.shape}"
assert np.isfinite(arr).all(), "non-finite values present"
assert (arr >= 0.0).all() and (arr < 1.0).all(), "values outside [0, 1)"
print(f"  count={COUNT}  all finite  all in [0,1)  PASS")

# ---------------------------------------------------------------------------
# 2. generate_uniform — custom low/high range
# ---------------------------------------------------------------------------
banner("2. generate_uniform(low=-5.0, high=5.0)")

view2 = dgen.generate_uniform(50_000, low=-5.0, high=5.0)
arr2 = as_numpy(view2, 50_000)
assert np.isfinite(arr2).all()
assert (arr2 >= -5.0).all() and (arr2 < 5.0).all(), "values outside [-5, 5)"
print("  custom range respected  PASS")

# ---------------------------------------------------------------------------
# 3. generate_uniform — low >= high must raise
# ---------------------------------------------------------------------------
banner("3. generate_uniform(low >= high) raises ValueError")

try:
    dgen.generate_uniform(100, low=1.0, high=1.0)
    raise AssertionError("expected ValueError for low == high")
except ValueError:
    print("  low == high correctly raised ValueError  PASS")

try:
    dgen.generate_uniform(100, low=2.0, high=1.0)
    raise AssertionError("expected ValueError for low > high")
except ValueError:
    print("  low > high correctly raised ValueError  PASS")

# ---------------------------------------------------------------------------
# 4. generate_uniform — seed determinism
# ---------------------------------------------------------------------------
banner("4. generate_uniform(seed=N) is deterministic")

a = as_numpy(dgen.generate_uniform(10_000, seed=1234), 10_000)
b = as_numpy(dgen.generate_uniform(10_000, seed=1234), 10_000)
assert np.array_equal(a, b), "same seed must produce identical output"
c = as_numpy(dgen.generate_uniform(10_000, seed=5678), 10_000)
assert not np.array_equal(a, c), "different seeds must produce different output"
print("  same seed -> identical; different seed -> different  PASS")

# ---------------------------------------------------------------------------
# 5. normalize_rows — unit L2 norm
# ---------------------------------------------------------------------------
banner("5. normalize_rows() — rows become unit L2 norm")

ROWS, DIM = 1_000, 128
buf = bytearray(dgen.generate_uniform(ROWS * DIM))
dgen.normalize_rows(buf, DIM)
mat = np.frombuffer(buf, dtype=np.float32).reshape(ROWS, DIM)
norms = np.linalg.norm(mat, axis=1)
assert np.allclose(norms, 1.0, atol=1e-3), f"norms not ~1.0: min={norms.min()} max={norms.max()}"
print(f"  {ROWS} rows all L2-normalized (~1.0)  PASS")

# ---------------------------------------------------------------------------
# 6. normalize_rows — in-place, no reallocation
# ---------------------------------------------------------------------------
banner("6. normalize_rows() mutates the SAME buffer object (zero-copy)")

buf2 = bytearray(dgen.generate_uniform(64 * 8))
buf2_id_before = id(buf2)
dgen.normalize_rows(buf2, 8)
assert id(buf2) == buf2_id_before, "normalize_rows must not reallocate the buffer"
print("  same bytearray object identity preserved  PASS")

# ---------------------------------------------------------------------------
# 7. normalize_rows — validation errors
# ---------------------------------------------------------------------------
banner("7. normalize_rows() input validation")

# read-only buffer (bytes, not bytearray) must raise
try:
    dgen.normalize_rows(bytes(dgen.generate_uniform(80)), 8)
    raise AssertionError("expected ValueError/TypeError for read-only buffer")
except (ValueError, TypeError):
    print("  read-only buffer correctly rejected  PASS")

# dim == 0 must raise
try:
    dgen.normalize_rows(bytearray(80), 0)
    raise AssertionError("expected ValueError for dim=0")
except ValueError:
    print("  dim=0 correctly rejected  PASS")

# length not a multiple of dim*4 must raise
try:
    dgen.normalize_rows(bytearray(81), 8)  # 81 not a multiple of 32
    raise AssertionError("expected ValueError for misaligned buffer length")
except ValueError:
    print("  misaligned buffer length correctly rejected  PASS")

# float64 numpy array must be rejected, not silently byte-reinterpreted
try:
    f64_arr = np.zeros(8, dtype=np.float64)
    dgen.normalize_rows(f64_arr, 8)
    raise AssertionError("expected ValueError for float64 array")
except (ValueError, TypeError):
    print("  float64 array correctly rejected (not silently reinterpreted)  PASS")

# ---------------------------------------------------------------------------
# 8. normalize_rows — zero row left unchanged, not NaN
# ---------------------------------------------------------------------------
banner("8. normalize_rows() leaves an all-zero row unchanged (no NaN)")

zero_buf = bytearray(8 * 4)  # one row of 8 zeros
dgen.normalize_rows(zero_buf, 8)
zero_arr = np.frombuffer(zero_buf, dtype=np.float32)
assert not np.isnan(zero_arr).any(), "zero row must not become NaN"
assert (zero_arr == 0.0).all(), "zero row must stay exactly zero"
print("  zero row stays zero, no NaN  PASS")

# ---------------------------------------------------------------------------
# 9. generate_uniform_vectors — fused, normalized by default
# ---------------------------------------------------------------------------
banner("9. generate_uniform_vectors() — fused generate+normalize")

ROWS2, DIM2 = 2_000, 256
view9 = dgen.generate_uniform_vectors(ROWS2, DIM2)
mat9 = np.frombuffer(memoryview(view9), dtype=np.float32).reshape(ROWS2, DIM2)
assert np.isfinite(mat9).all()
norms9 = np.linalg.norm(mat9, axis=1)
assert np.allclose(norms9, 1.0, atol=1e-3)
print(f"  {ROWS2}x{DIM2} vectors, all finite, all unit-normalized  PASS")

# ---------------------------------------------------------------------------
# 10. generate_uniform_vectors — normalize=False skips normalization
# ---------------------------------------------------------------------------
banner("10. generate_uniform_vectors(normalize=False)")

view10 = dgen.generate_uniform_vectors(200, 32, normalize=False)
mat10 = np.frombuffer(memoryview(view10), dtype=np.float32).reshape(200, 32)
norms10 = np.linalg.norm(mat10, axis=1)
assert not np.allclose(norms10, 1.0, atol=1e-3), "normalize=False must leave rows un-normalized"
print("  normalize=False correctly skips normalization  PASS")

# ---------------------------------------------------------------------------
# 11. generate_uniform_vectors — equivalent to separate calls (same seed)
# ---------------------------------------------------------------------------
banner("11. generate_uniform_vectors() == generate_uniform + normalize_rows (same seed)")

ROWS3, DIM3, SEED = 500, 96, 777
fused = bytes(dgen.generate_uniform_vectors(ROWS3, DIM3, seed=SEED))

separate = bytearray(dgen.generate_uniform(ROWS3 * DIM3, seed=SEED))
dgen.normalize_rows(separate, DIM3)

assert fused == bytes(separate), "fused path must be byte-for-byte equivalent to separate calls"
print("  fused output byte-for-byte equals separate generate_uniform + normalize_rows  PASS")

# ---------------------------------------------------------------------------
# 12. generate_uniform_vectors — VDB production shape smoke test
# ---------------------------------------------------------------------------
banner("12. generate_uniform_vectors() at VDB production shape (10k x 1536)")

view12 = dgen.generate_uniform_vectors(10_000, 1536)
mat12 = np.frombuffer(memoryview(view12), dtype=np.float32).reshape(10_000, 1536)
assert np.isfinite(mat12).all()
assert np.allclose(np.linalg.norm(mat12, axis=1), 1.0, atol=1e-3)
print("  10,000 x 1536 vectors generated and normalized correctly  PASS")

print(f"\n{'='*60}")
print("  ALL NUMERIC DISTRIBUTION TESTS PASSED")
print(f"{'='*60}")
