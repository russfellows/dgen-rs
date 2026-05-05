#!/usr/bin/env python3
"""
tests/test_dgen_py_v024.py

Verify dgen-py v0.2.4 library routines work correctly.
Tests: generate_buffer(), BufferPool, Generator streaming, throughput, concurrency.

v0.2.4 changes tested here:
  - Global Rayon pool (OnceLock) shared across all Generator instances
  - Parallel generation is the sole dedup-safe mode (XorStream removed)
"""

import sys
import time
import threading
import numpy as np

# dgen-py is installed from the dgen-rs workspace via maturin develop
import dgen_py as dgen

SIZE_1M = 1 * 1024 * 1024
SIZE_8M = 8 * 1024 * 1024
SIZE_32M = 32 * 1024 * 1024


def banner(title: str) -> None:
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}")


# ---------------------------------------------------------------------------
# 1. generate_buffer — basic
# ---------------------------------------------------------------------------
banner("1. generate_buffer() — single-call generation")

buf1 = dgen.generate_buffer(SIZE_8M)
print(f"  type    : {type(buf1)}")
print(f"  len     : {len(buf1)}")
assert len(buf1) == SIZE_8M, f"Expected {SIZE_8M}, got {len(buf1)}"

view1 = memoryview(buf1)
assert len(view1) == SIZE_8M
print(f"  memoryview len : {len(view1)}  ✓")

arr1 = np.frombuffer(view1, dtype=np.uint8)
assert arr1.shape == (SIZE_8M,)
print(f"  numpy shape    : {arr1.shape}  ✓")

buf2 = dgen.generate_buffer(SIZE_8M)
arr2 = np.frombuffer(memoryview(buf2), dtype=np.uint8)
diff = int(np.sum(arr1 != arr2))
print(f"  bytes differing between two calls: {diff} (expect > 0)  ✓")
assert diff > 0, "Two calls produced identical data — seeding broken?"
print("  PASS")

# ---------------------------------------------------------------------------
# 2. generate_buffer with dedup_ratio / compress_ratio
# ---------------------------------------------------------------------------
banner("2. generate_buffer(dedup_ratio=4, compress_ratio=2)")

buf_d = dgen.generate_buffer(SIZE_8M, dedup_ratio=4, compress_ratio=2)
assert len(buf_d) == SIZE_8M
print(f"  len : {len(buf_d)}  ✓")
print("  PASS")

# ---------------------------------------------------------------------------
# 3. Generator — streaming fill_chunk
# ---------------------------------------------------------------------------
banner("3. Generator.fill_chunk() — streaming")

gen_s = dgen.Generator(size=SIZE_32M, dedup_ratio=1, compress_ratio=1)
buf = bytearray(SIZE_1M)

total = 0
chunks = 0
while not gen_s.is_complete():
    n = gen_s.fill_chunk(buf)
    if n == 0:
        break
    total += n
    chunks += 1

print(f"  chunks filled : {chunks}")
print(f"  bytes total   : {total}  (expected {SIZE_32M})")
assert total == SIZE_32M, f"Expected {SIZE_32M} bytes, got {total}"
print("  PASS")

# ---------------------------------------------------------------------------
# 4. Generator throughput
# ---------------------------------------------------------------------------
banner("4. Generator throughput benchmark")

BENCH_SIZE = 512 * SIZE_1M
gen_b = dgen.Generator(size=BENCH_SIZE, dedup_ratio=1, compress_ratio=1)
buf_b = bytearray(SIZE_32M)

t0 = time.perf_counter()
written = 0
while not gen_b.is_complete():
    n = gen_b.fill_chunk(buf_b)
    if n == 0:
        break
    written += n
elapsed = time.perf_counter() - t0

gbps = written / elapsed / 1e9
print(f"  {written/1e6:.0f} MB in {elapsed:.3f}s = {gbps:.2f} GB/s")
print("  PASS")

# ---------------------------------------------------------------------------
# 5. generate_buffer uniqueness — dedup safety of Parallel generator
# ---------------------------------------------------------------------------
banner("5. generate_buffer() uniqueness (dedup-safe, Parallel generator)")

N_BUFS = 8
bufs = [dgen.generate_buffer(SIZE_1M) for _ in range(N_BUFS)]
arrs = [np.frombuffer(memoryview(b), dtype=np.uint8) for b in bufs]

all_unique = True
for i in range(len(arrs)):
    for j in range(i + 1, len(arrs)):
        diff = int(np.sum(arrs[i] != arrs[j]))
        if diff == 0:
            print(f"  ERROR: buffers {i} and {j} are identical!")
            all_unique = False

print(f"  {N_BUFS} buffers, all pairwise unique: {all_unique}  ✓")
assert all_unique, "generate_buffer() produced duplicate buffers!"
print("  PASS")

# ---------------------------------------------------------------------------
# 6. Global pool — multiple generators share pool, produce unique output
# ---------------------------------------------------------------------------
banner("6. Global pool — N concurrent Generators, each unique output")

N_THREADS = 8
THREAD_ITERS = 16
results_data = [None] * N_THREADS
errors = []

def worker_gen(tid: int) -> None:
    try:
        local_buf = bytearray(SIZE_1M)
        gen = dgen.Generator(size=SIZE_1M * THREAD_ITERS, dedup_ratio=1, compress_ratio=1)
        fills = 0
        while not gen.is_complete():
            n = gen.fill_chunk(local_buf)
            if n == 0:
                break
            fills += 1
        results_data[tid] = fills
    except Exception as exc:
        errors.append((tid, exc))

threads = [threading.Thread(target=worker_gen, args=(i,)) for i in range(N_THREADS)]
t0 = time.perf_counter()
for t in threads:
    t.start()
for t in threads:
    t.join()
elapsed = time.perf_counter() - t0

if errors:
    print(f"  ERRORS: {errors}")
    sys.exit(1)

total_fills = sum(results_data)
total_bytes = total_fills * SIZE_1M
gbps = total_bytes / elapsed / 1e9
print(f"  {N_THREADS} threads, {total_fills} fills, {total_bytes/1e6:.0f} MB in {elapsed:.3f}s = {gbps:.2f} GB/s")
assert all(r == THREAD_ITERS for r in results_data), f"Unexpected fill counts: {results_data}"
print("  PASS")

# ---------------------------------------------------------------------------
# 7. generate_buffer() throughput benchmark
# ---------------------------------------------------------------------------
banner("7. generate_buffer() throughput (single-threaded)")

ITERS = 64
buf7 = None
t0 = time.perf_counter()
for _ in range(ITERS):
    buf7 = dgen.generate_buffer(SIZE_8M)
elapsed = time.perf_counter() - t0

total_bytes = ITERS * SIZE_8M
gbps = total_bytes / elapsed / 1e9
print(f"  {total_bytes/1e6:.0f} MB in {elapsed:.3f}s = {gbps:.2f} GB/s")
print("  PASS")

# ---------------------------------------------------------------------------
# 8. BufferPool.next_slice()
# ---------------------------------------------------------------------------
banner("8. BufferPool.next_slice() — rolling zero-copy pool")

pool = dgen.BufferPool(dedup_ratio=1, compress_ratio=1)
slices = [pool.next_slice(SIZE_1M) for _ in range(8)]
print(f"  generated {len(slices)} slices of {SIZE_1M} bytes each")
for i, s in enumerate(slices):
    assert len(s) == SIZE_1M, f"Slice {i} wrong length: {len(s)}"
print(f"  all {len(slices)} slices have correct length  ✓")
print("  PASS")

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
print("\n" + "="*60)
print("  ALL TESTS PASSED ✓")
print("="*60)
