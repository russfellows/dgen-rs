#!/usr/bin/env python3
"""
v0.2.3 Small-Object Speed Test
================================
Verifies the rolling pool automatic fast path (generate_buffer) and the
explicit BufferPool class introduced in dgen-py v0.2.3.

Tests
-----
1. generate_buffer correctness  — right size, non-zero data, is a BytesView
2. BufferPool correctness       — right size, non-zero, independent slices differ
3. generate_buffer small-object speed  — 64 B, 512 B, 4 KB, 64 KB object sizes
4. BufferPool small-object speed       — same sizes, for direct comparison
5. Rolling pool vs large-object        — confirm 1 MB and 256 MB use cases
6. BufferPool.reconfigure()            — new pool generates different data
"""

import sys
import time
import hashlib

try:
    import dgen_py
    from dgen_py import BufferPool, BytesView, generate_buffer
except ImportError:
    print("ERROR: dgen-py is not installed.")
    sys.exit(1)

try:
    import numpy as np
    # Use the modern Generator API (NumPy 1.17+, recommended over legacy np.random.*)
    # PCG64 is the default — fast, high quality, suitable for large datasets
    _np_rng = np.random.default_rng()
    HAS_NUMPY = True
except ImportError:
    HAS_NUMPY = False
    print("WARNING: numpy not available, skipping NumPy comparisons")

PASS = "✅ PASS"
FAIL = "❌ FAIL"

failures = 0

def section(title):
    print(f"\n{'─' * 70}")
    print(f"  {title}")
    print('─' * 70)

def check(label, cond, detail=""):
    global failures
    status = PASS if cond else FAIL
    if not cond:
        failures += 1
    suffix = f"  ({detail})" if detail else ""
    print(f"  {status}  {label}{suffix}")
    return cond

def throughput(total_bytes, elapsed_s):
    gb = total_bytes / 1e9
    if gb / elapsed_s >= 1.0:
        return f"{gb / elapsed_s:.2f} GB/s"
    return f"{gb / elapsed_s * 1000:.0f} MB/s"

# ─────────────────────────────────────────────────────────────────────────────
print(f"\n{'=' * 70}")
print(f"  dgen-py v{dgen_py.__version__}  Small-Object & BufferPool Test")
print(f"{'=' * 70}")

# ── 1. generate_buffer correctness ───────────────────────────────────────────
section("1. generate_buffer() correctness")

for size in [64, 512, 4096, 64 * 1024, 1024 * 1024]:
    buf = generate_buffer(size)
    check(f"size={size:>8}: correct length",   len(buf) == size,      f"got {len(buf)}")
    check(f"size={size:>8}: returns BytesView", isinstance(buf, BytesView))
    check(f"size={size:>8}: non-zero data",    any(b != 0 for b in bytes(buf)[:64]))

# ── 2. BufferPool correctness ─────────────────────────────────────────────────
section("2. BufferPool() correctness")

pool = BufferPool()
check("BufferPool(): creates instance", pool is not None)
check("BufferPool().remaining <= 1 MB", pool.remaining <= 1024 * 1024)
check("BufferPool().dedup_ratio == 1",   pool.dedup_ratio == 1)
check("BufferPool().compress_ratio == 1", pool.compress_ratio == 1)

s1 = pool.next_slice(64 * 1024)
s2 = pool.next_slice(64 * 1024)
check("next_slice(64KB): correct length",   len(s1) == 64 * 1024)
check("next_slice(64KB): returns BytesView", isinstance(s1, BytesView))
check("next_slice: consecutive slices differ", bytes(s1) != bytes(s2),
      "two adjacent 64 KB windows should contain different data")

remaining_before = pool.remaining
_ = pool.next_slice(4096)
check("remaining decrements after next_slice", pool.remaining < remaining_before)

# reconfigure with same values → no refill, remaining unchanged
rem_before = pool.remaining
pool.reconfigure(dedup_ratio=1.0, compress_ratio=1.0)
check("reconfigure (no change): remaining unchanged", pool.remaining == rem_before)

# reconfigure with new values → refill, remaining resets to near 1 MB
pool.reconfigure(dedup_ratio=1.0, compress_ratio=2.0)
check("reconfigure (changed): pool refilled", pool.remaining > rem_before or pool.remaining > 900_000,
      f"remaining={pool.remaining}")
check("compress_ratio updated", pool.compress_ratio == 2)

# In-flight slices survive a reconfigure
pool2 = BufferPool()
held = pool2.next_slice(64 * 1024)       # grab a slice
pool2.reconfigure(compress_ratio=3.0)    # force refill
check("in-flight slice valid after reconfigure", len(held) == 64 * 1024)

# Zero-size should raise ValueError
try:
    pool.next_slice(0)
    check("next_slice(0) raises ValueError", False, "no exception raised")
except ValueError:
    check("next_slice(0) raises ValueError", True)

# ── 3 & 4. Speed comparison ────────────────────────────────────────────────────
section("3. Speed: dgen-py BufferPool vs NumPy PCG64")

if HAS_NUMPY:
    print(f"  NumPy {np.__version__}  —  numpy.random.default_rng() [PCG64]")
    print(f"  Generating uint8 arrays:  rng.integers(0, 256, size=N, dtype=np.uint8)")
else:
    print("  NumPy not available — skipping comparison column")
print(f"  (For pure-Rust numbers: cargo run --release --example speed-table)")

# 1 GiB total data per scenario; for obj_size > 1 GiB exactly 1 call is made
TOTAL = 1024 * 1024 * 1024

SIZES = [
    (          64, "64 B    "),
    (         512, "512 B   "),
    (        4096, "4 KB    "),
    (      65_536, "64 KB   "),
    (   1_048_576, "1 MB    "),
    (  10_485_760, "10 MB   "),
    ( 104_857_600, "100 MB  "),
    (1_073_741_824, "1 GB    "),
    (10_737_418_240, "10 GB   "),
]

if HAS_NUMPY:
    hdr = f"  {'Object':<9}  {'BufferPool (py)':>16}  {'NumPy PCG64':>14}  {'vs NumPy':>9}"
    sep = f"  {'--------':<9}  {'--------------':>16}  {'-----------':>14}  {'--------':>9}"
else:
    hdr = f"  {'Object':<9}  {'BufferPool (py)':>16}"
    sep = f"  {'--------':<9}  {'--------------':>16}"
print(f"\n{hdr}")
print(sep)

for obj_size, label in SIZES:
    calls = max(TOTAL // obj_size, 1)
    do_warmup = obj_size <= 1_048_576   # skip warmup iteration for large objects

    # ── BufferPool (Python via PyO3) ──────────────────────────────────────────
    if do_warmup:
        _warmup_pool = BufferPool()
        _warmup_pool.next_slice(obj_size)

    pool3 = BufferPool()
    t0 = time.perf_counter()
    total_bytes2 = 0
    for _ in range(calls):
        b = pool3.next_slice(obj_size)
        total_bytes2 += len(b)
    bp_elapsed = time.perf_counter() - t0
    bp_tput = throughput(total_bytes2, bp_elapsed)

    # ── NumPy PCG64 ───────────────────────────────────────────────────────
    if HAS_NUMPY:
        rng = np.random.default_rng()
        np_elapsed = None
        np_tput = "N/A"
        try:
            if do_warmup:
                rng.integers(0, 256, size=obj_size, dtype=np.uint8)
            t0 = time.perf_counter()
            total_bytes3 = 0
            for _ in range(calls):
                arr = rng.integers(0, 256, size=obj_size, dtype=np.uint8)
                total_bytes3 += len(arr)
            np_elapsed = time.perf_counter() - t0
            np_tput = throughput(total_bytes3, np_elapsed)
        except MemoryError:
            np_tput = "MemoryError"

        vs_numpy = f"{np_elapsed / bp_elapsed:>8.2f}×" if np_elapsed is not None else "N/A"
        print(f"  {label}  {bp_tput:>16}  {np_tput:>14}  {vs_numpy:>9}")
    else:
        print(f"  {label}  {bp_tput:>16}")


# ── 5. Large-object bypass (>1 MB) —  both APIs should be comparable ──────────
section("4. Large-object bypass  (BufferPool > 1 MB passes through directly)")

for obj_size, label in [(4*1024*1024, "4 MB"), (64*1024*1024, "64 MB")]:
    buf = generate_buffer(obj_size)
    check(f"generate_buffer({label}): correct length", len(buf) == obj_size)
    pool4 = BufferPool()
    sl = pool4.next_slice(obj_size)
    check(f"BufferPool.next_slice({label}): correct length", len(sl) == obj_size)

# ─────────────────────────────────────────────────────────────────────────────
section("SUMMARY")
if failures == 0:
    print(f"\n  🎉 All tests passed!  (dgen-py v{dgen_py.__version__})")
else:
    print(f"\n  {failures} test(s) FAILED.")
    sys.exit(1)
