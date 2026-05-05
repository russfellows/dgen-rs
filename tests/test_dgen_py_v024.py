#!/usr/bin/env python3
"""
tests/test_dgen_py_v024.py

Verify dgen-py v0.2.4 library routines work correctly.
Tests: generate_buffer(), BufferPool, Generator, XorStream.fill(), XorStream.generate()
Zero-copy: memoryview(), numpy.frombuffer()
Threading: multiple threads sharing one XorStream
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
# 5. XorStream.fill() — in-place zero-copy
# ---------------------------------------------------------------------------
banner("5. XorStream.fill() — in-place generation")

stream = dgen.XorStream()
buf_x = bytearray(SIZE_8M)
stream.fill(buf_x)

arr_x = np.frombuffer(buf_x, dtype=np.uint8)
print(f"  non-zero bytes: {np.count_nonzero(arr_x)}")
assert np.count_nonzero(arr_x) > 0, "fill() produced all zeros!"

buf_x2 = bytearray(SIZE_8M)
stream.fill(buf_x2)
arr_x2 = np.frombuffer(buf_x2, dtype=np.uint8)
arr_x1_fresh = np.frombuffer(buf_x, dtype=np.uint8)
diff_x = int(np.sum(arr_x1_fresh != arr_x2))
print(f"  bytes differing between two fills: {diff_x} (expect > 0)  ✓")
assert diff_x > 0, "Two fills produced identical data!"

print(f"  objects_generated : {stream.objects_generated}")
assert stream.objects_generated == 2
print("  PASS")

# ---------------------------------------------------------------------------
# 6. XorStream.generate() — BytesView zero-copy
# ---------------------------------------------------------------------------
banner("6. XorStream.generate() — BytesView zero-copy")

stream2 = dgen.XorStream()
data_g = stream2.generate(SIZE_8M)
print(f"  type : {type(data_g)}")
assert len(data_g) == SIZE_8M

view_g = memoryview(data_g)
arr_g = np.frombuffer(view_g, dtype=np.uint8)
assert arr_g.shape == (SIZE_8M,)
print(f"  numpy shape : {arr_g.shape}  ✓")

print(f"  objects_generated : {stream2.objects_generated}")
assert stream2.objects_generated == 1
print("  PASS")

# ---------------------------------------------------------------------------
# 7. XorStream throughput
# ---------------------------------------------------------------------------
banner("7. XorStream.fill() throughput benchmark")

stream3 = dgen.XorStream()
buf3 = bytearray(SIZE_8M)
ITERS = 64

t0 = time.perf_counter()
for _ in range(ITERS):
    stream3.fill(buf3)
elapsed = time.perf_counter() - t0

total_bytes = ITERS * SIZE_8M
gbps = total_bytes / elapsed / 1e9
print(f"  {total_bytes/1e6:.0f} MB in {elapsed:.3f}s = {gbps:.2f} GB/s")
print("  PASS")

# ---------------------------------------------------------------------------
# 8. Threading — multiple threads sharing one XorStream
# ---------------------------------------------------------------------------
banner("8. Multi-thread safety of XorStream (shared instance)")

N_THREADS = 8
THREAD_ITERS = 32
stream_shared = dgen.XorStream()
results = [None] * N_THREADS
errors = []

def worker(tid: int) -> None:
    try:
        local_buf = bytearray(SIZE_1M)
        for _ in range(THREAD_ITERS):
            stream_shared.fill(local_buf)
        results[tid] = True
    except Exception as exc:
        errors.append((tid, exc))

threads = [threading.Thread(target=worker, args=(i,)) for i in range(N_THREADS)]
for t in threads:
    t.start()
for t in threads:
    t.join()

if errors:
    print(f"  ERRORS: {errors}")
    sys.exit(1)

assert all(r is True for r in results), f"Some threads failed: {results}"
total_fills = N_THREADS * THREAD_ITERS
print(f"  {N_THREADS} threads × {THREAD_ITERS} fills = {total_fills} total fills  ✓")
print(f"  objects_generated : {stream_shared.objects_generated}")
assert stream_shared.objects_generated == total_fills
print("  PASS")

# ---------------------------------------------------------------------------
# 9. BufferPool.next_slice()
# ---------------------------------------------------------------------------
banner("9. BufferPool.next_slice() — rolling zero-copy pool")

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
