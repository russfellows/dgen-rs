#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# dgen-py speed table — Python mirror of examples/speed_table.rs
#
# Two columns matching the two Rust columns exactly:
#
#  Per-object — one API call per object in a tight loop
#    ≤ 1 MB : BufferPool.next_slice(N)
#               Pool created once, zero-copy Bytes slice per call.
#               This is how sai3-bench calls dgen-rs for small objects
#               (thread-local RollingPool).
#    > 1 MB : Generator(size=N).get_chunk(N)  — new Generator per call.
#               This is EXACTLY how dlio_benchmark calls dgen-py:
#               gen = dgen_py.Generator(size=total_bytes, seed=seed)
#               bytesview = gen.get_chunk(total_bytes)
#               Each call creates a fresh DataGenerator + Rayon thread pool.
#
#  Streaming — one Generator for the entire measurement volume, 32 MB chunks
#               Thread pool created ONCE in Generator.__init__() and reused
#               for every fill_chunk() call.  Matches how you'd build a
#               high-throughput data pipeline.
#
# Usage:
#   source .venv/bin/activate   # or wherever dgen-py is installed
#   python examples/speed_table.py

import sys
import time
import os

try:
    import dgen_py
    from dgen_py import BufferPool, Generator
except ImportError:
    print("ERROR: dgen-py not installed.  Run:  pip install dgen-py")
    print("       or:  source .venv/bin/activate")
    sys.exit(1)

# ── Match the same constants used in speed_table.rs ─────────────────────────

BLOCK_SIZE = 1024 * 1024          # 1 MB (same as dgen_data BLOCK_SIZE)

PO_WARMUP  = 0.500                # 500 ms warmup for per-object column
PO_MEASURE = 2.000                # 2 s measurement window
# Minimum iteration count for the per-object column.
# For large objects (e.g. 10 GB ≈ 500 ms each) the time window alone captures
# only 3–4 cold-start calls.  Enforcing this minimum ensures ≥ 10
# steady-state calls so the average is representative.
PO_MIN_ITERS = 10

STREAM_CHUNK       = 32 * 1024 * 1024       # 32 MB chunk — optimal fill_chunk() size
STREAM_WARMUP_TOTAL = 1 * 1024 ** 3         # 1 GB warmup volume
STREAM_MEASURE_MIN  = 10 * 1024 ** 3        # 10 GB minimum measurement
STREAM_MEASURE_MAX  = 20 * 1024 ** 3        # 20 GB maximum measurement

SIZES = [
    (           64, "64 B  "),
    (          512, "512 B "),
    (     4 * 1024, "4 KB  "),
    (    64 * 1024, "64 KB "),
    (  1024 * 1024, "1 MB  "),
    ( 10 * 1024**2, "10 MB "),
    (100 * 1024**2, "100 MB"),
    (     1024**3,  "1 GB  "),
    ( 10 * 1024**3, "10 GB "),
]

# ── Helpers ──────────────────────────────────────────────────────────────────

def fmt_tput(total_bytes, elapsed_s):
    gb_s = total_bytes / elapsed_s / 1e9
    if gb_s >= 1.0:
        return f"{gb_s:.2f} GB/s"
    return f"{gb_s * 1000:.0f} MB/s"

# ── Column 1: Per-object ─────────────────────────────────────────────────────

def bench_per_object(obj_bytes):
    """
    Benchmark one-call-per-object throughput.

    Small objects (≤ 1 MB):
        BufferPool.next_slice(N) — pool created once and reused across all
        iterations.  Matches sai3-bench's thread-local RollingPool pattern:
        a single 1 MB block is pre-generated; each next_slice() returns a
        zero-copy Bytes window.  Pool refills only on exhaustion.

    Large objects (> 1 MB):
        Generator(size=N).get_chunk(N) per iteration — new Generator (and
        therefore new DataGenerator + Rayon thread pool) every call.  This is
        EXACTLY the pattern used by dlio_benchmark for checkpoint generation:
            gen = dgen_py.Generator(size=total_bytes, seed=seed)
            bytesview = gen.get_chunk(total_bytes)
        The per-call pool setup cost is included in the measurement.
    """
    if obj_bytes <= BLOCK_SIZE:
        # ── Small-object path: BufferPool ────────────────────────────────────
        # Warmup: run until PO_WARMUP has elapsed (≥ 1 call always finishes)
        pool_w = BufferPool()
        deadline = time.perf_counter() + PO_WARMUP
        while True:
            pool_w.next_slice(obj_bytes)
            if time.perf_counter() >= deadline:
                break

        # Measurement: per-call timer, buffer released after timer stops.
        pool = BufferPool()
        gen_secs = 0.0
        generated = 0
        iters = 0
        wall_deadline = time.perf_counter() + PO_MEASURE
        while True:
            t0 = time.perf_counter()
            b = pool.next_slice(obj_bytes)
            gen_secs += time.perf_counter() - t0   # stop timer before del
            generated += len(b)
            iters += 1
            del b                                   # free/munmap untimed
            if iters >= PO_MIN_ITERS and time.perf_counter() >= wall_deadline:
                break
    else:
        # ── Large-object path: new Generator per call ────────────────────────
        # Warmup
        deadline = time.perf_counter() + PO_WARMUP
        while True:
            gen = Generator(size=obj_bytes)
            gen.get_chunk(obj_bytes)
            if time.perf_counter() >= deadline:
                break

        # Measurement: per-call timer, buffer released after timer stops.
        # For 10 GB objects (~500 ms each) this enforces ≥ 10 steady-state
        # calls so the average isn't dominated by 3–4 cold-start iterations.
        gen_secs = 0.0
        generated = 0
        iters = 0
        wall_deadline = time.perf_counter() + PO_MEASURE
        while True:
            t0 = time.perf_counter()
            g = Generator(size=obj_bytes)
            bv = g.get_chunk(obj_bytes)
            gen_secs += time.perf_counter() - t0   # stop timer before del
            if bv is not None:
                generated += len(bv)
            iters += 1
            del bv, g                               # munmap untimed
            if iters >= PO_MIN_ITERS and time.perf_counter() >= wall_deadline:
                break

    return generated, gen_secs

# ── Column 2: NumPy per-object ──────────────────────────────────────────────

NP_MIN_ITERS = 3   # numpy is single-threaded; 3 iters gives a stable average
                   # for large objects without requiring 10 × ~5 s calls

def bench_numpy_per_object(obj_bytes):
    """
    Benchmark numpy random data generation using the same per-call timing method.

    Uses np.random.default_rng() (PCG64) created ONCE before the loop — the
    natural NumPy pattern for a tight generation loop.  The array is deleted
    (freed) AFTER the timer stops, exactly like the dgen-py and Rust columns.

    float64 random is the fastest NumPy path: each element is 8 bytes, so
    obj_bytes // 8 elements == obj_bytes of random data.  All SIZES are powers
    of 2 so the division is exact.
    """
    import numpy as np
    n_floats = obj_bytes // 8
    rng = np.random.default_rng()

    # Warmup
    deadline = time.perf_counter() + PO_WARMUP
    while True:
        arr = rng.random(size=n_floats)
        del arr
        if time.perf_counter() >= deadline:
            break

    # Measurement: per-call timer, array freed after timer stops
    gen_secs = 0.0
    generated = 0
    iters = 0
    wall_deadline = time.perf_counter() + PO_MEASURE
    while True:
        t0 = time.perf_counter()
        arr = rng.random(size=n_floats)
        gen_secs += time.perf_counter() - t0   # stop timer before del
        generated += arr.nbytes
        iters += 1
        del arr                                 # free untimed
        if iters >= NP_MIN_ITERS and time.perf_counter() >= wall_deadline:
            break

    return generated, gen_secs


# ── Column 3: Streaming ──────────────────────────────────────────────────────

def bench_streaming(obj_bytes):
    """
    Benchmark continuous bulk-generation throughput.

    One Generator is created for the entire measurement volume.  fill_chunk()
    is called repeatedly with a fixed 32 MB bytearray until the Generator is
    exhausted.  The Rayon thread pool inside the Generator is created once in
    Generator.__init__() and reused for every fill_chunk() call — no per-call
    pool startup cost.

    This is the pattern for high-throughput data pipelines: generate all data
    needed for a benchmark run in one continuous pass.
    """
    measure_total = min(max(STREAM_MEASURE_MIN, obj_bytes * 4), STREAM_MEASURE_MAX)
    warmup_total  = min(STREAM_WARMUP_TOTAL, measure_total)

    buf = bytearray(STREAM_CHUNK)

    # Warmup
    gen_w = Generator(size=warmup_total)
    while not gen_w.is_complete():
        gen_w.fill_chunk(buf)

    # Measurement
    gen = Generator(size=measure_total)
    t0 = time.perf_counter()
    while not gen.is_complete():
        gen.fill_chunk(buf)
    elapsed = time.perf_counter() - t0

    return measure_total, elapsed

# ── Main ─────────────────────────────────────────────────────────────────────

def main():
    ncpus = os.cpu_count() or 1
    sep   = "─" * 80

    try:
        import numpy as np
        has_numpy = True
    except ImportError:
        has_numpy = False

    print()
    print(f"  dgen-py v{dgen_py.__version__} speed table  ({ncpus} logical CPUs)  — Python via PyO3")
    print()
    print("  Per-object  — one API call per object in a loop, munmap outside timer")
    print("    ≤ 1 MB  BufferPool.next_slice(): pool created once, zero-copy per call")
    print("    > 1 MB  Generator(size=N).get_chunk(N): new Generator per call")
    if has_numpy:
        print("  NumPy       — np.random.default_rng().random(N//8), rng reused each loop")
        print("                PCG64 float64; single-threaded; del arr outside timer")
    else:
        print("  NumPy       — (not installed; install with: pip install dgen-py[numpy])")
    print()
    print("  Streaming   — Generator.fill_chunk(), 32 MB chunks")
    print("    One Generator for full volume; thread pool reused every fill_chunk().")
    print()
    print(f"  Per-object warmup: {PO_WARMUP*1000:.0f} ms  measurement: ≥ {PO_MEASURE*1000:.0f} ms per row")
    print(f"  Streaming  warmup: {STREAM_WARMUP_TOTAL // 1024**3} GB  "
          f"measurement: {STREAM_MEASURE_MIN // 1024**3}–{STREAM_MEASURE_MAX // 1024**3} GB per row")
    print()
    if has_numpy:
        print(f"  {'Object':<8}  {'Per-object (py)':>16}  {'Per-object (np)':>16}  {'Streaming (py)':>16}")
    else:
        print(f"  {'Object':<8}  {'Per-object (py)':>16}  {'Streaming (py)':>16}")
    print(f"  {sep}")

    for obj_bytes, label in SIZES:
        po_bytes, po_secs = bench_per_object(obj_bytes)
        st_bytes, st_secs = bench_streaming(obj_bytes)
        if has_numpy:
            np_bytes, np_secs = bench_numpy_per_object(obj_bytes)
            print(f"  {label:<8}  {fmt_tput(po_bytes, po_secs):>16}  {fmt_tput(np_bytes, np_secs):>16}  {fmt_tput(st_bytes, st_secs):>16}")
        else:
            print(f"  {label:<8}  {fmt_tput(po_bytes, po_secs):>16}  {fmt_tput(st_bytes, st_secs):>16}")

    print()
    print("  Notes:")
    if has_numpy:
        print("    NumPy (PCG64) is single-threaded; dgen-py uses Xoshiro256++ across all")
        print("    Rayon threads.  NumPy throughput is memory-bandwidth-limited for large")
        print("    objects on a single thread; dgen-py fills all cores in parallel.")
    print("    Streaming throughput is independent of 'object size' (always 32 MB chunks).")
    print()
    print("  Compare with pure-Rust columns:")
    print("    cargo run --release --example speed-table")
    print()

if __name__ == "__main__":
    main()
