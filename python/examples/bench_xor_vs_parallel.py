#!/usr/bin/env python3
"""
XorStream vs Parallel — dgen-py Generation Method Benchmark
=============================================================
Measures throughput (GB/s) and CPU utilisation for both generation methods
across a range of object sizes and thread counts.

Methods under test
------------------
  parallel   — Rayon + Xoshiro256++ (current default)
  xor        — UniqueXorStream keystream (new, single-threaded by design)

APIs exercised
--------------
  generate_buffer()  — allocates and returns a new buffer
  Generator (streaming fill_chunk)  — fills a pre-allocated bytearray

Test matrix
-----------
  Object sizes : 64 KB, 512 KB, 1 MB, 4 MB, 16 MB, 64 MB, 256 MB
  Thread counts: 1, 4, all-cores  (parallel only — xor is always 1 thread)
  Repeats      : enough to accumulate ≥ 2 GB per cell (min 4 reps)

CPU measurement
---------------
Uses /proc/self/stat (kernel + user jiffies) bracketed around each timed
block so we get actual CPU-seconds consumed, not wall-clock × cores.
Reported as "CPU efficiency" = data_GB / cpu_seconds.

Usage
-----
    python bench_xor_vs_parallel.py [--size-gb N] [--no-streaming]

    --size-gb N      total GB to generate per cell (default: 2)
    --no-streaming   skip the streaming (fill_chunk) section
"""

import argparse
import os
import sys
import time
import multiprocessing

try:
    import dgen_py
except ImportError:
    print("ERROR: dgen-py is not installed.")
    print("  cd /home/eval/Documents/Code/dgen-rs")
    print("  source .venv/bin/activate")
    print("  maturin develop --release")
    sys.exit(1)

# ── helpers ───────────────────────────────────────────────────────────────────

JIFFY_HZ = os.sysconf("SC_CLK_TCK")   # usually 100

def _read_cpu_jiffies() -> int:
    """Return total (user + kernel) jiffies consumed by this process."""
    with open("/proc/self/stat") as f:
        fields = f.read().split()
    # fields[13] = utime, fields[14] = stime (0-indexed)
    return int(fields[13]) + int(fields[14])


def cpu_seconds_elapsed(j0: int, j1: int) -> float:
    return (j1 - j0) / JIFFY_HZ


def fmt_bw(gb_s: float) -> str:
    return f"{gb_s:7.2f} GB/s"


def fmt_cpu(cpu_eff: float) -> str:
    """GB per CPU-second — higher means less CPU burned per byte generated."""
    return f"{cpu_eff:7.2f} GB/CPU·s"


def required_reps(size_bytes: int, target_gb: float) -> int:
    target_bytes = target_gb * 1e9
    return max(4, -(-int(target_bytes) // size_bytes))   # ceiling division


# ── generate_buffer benchmarks ────────────────────────────────────────────────

def bench_generate_buffer(size: int, method: str, threads=None,
                           target_gb: float = 2.0) -> dict:
    """Benchmark generate_buffer for a given size/method/thread-count."""
    reps = required_reps(size, target_gb)
    kwargs = dict(method=method)
    if threads is not None and method == "parallel":
        kwargs["max_threads"] = threads

    # warm-up (not timed)
    _ = dgen_py.generate_buffer(size, **kwargs)

    j0 = _read_cpu_jiffies()
    t0 = time.perf_counter()
    for _ in range(reps):
        _ = dgen_py.generate_buffer(size, **kwargs)
    elapsed = time.perf_counter() - t0
    cpu_s = cpu_seconds_elapsed(j0, _read_cpu_jiffies())

    total_gb = size * reps / 1e9
    return {
        "gb_s":    total_gb / elapsed,
        "cpu_eff": total_gb / cpu_s if cpu_s > 0 else float("inf"),
        "cpu_s":   cpu_s,
        "elapsed": elapsed,
        "reps":    reps,
        "total_gb": total_gb,
    }


# ── streaming (fill_chunk) benchmarks ─────────────────────────────────────────

def bench_streaming(size: int, method: str, threads=None,
                    target_gb: float = 2.0) -> dict:
    """
    Benchmark Generator.fill_chunk() for a given size/method/thread-count.

    Uses the object size as both the generator total_size and the chunk size so
    each fill_chunk() call produces exactly one object's worth of data.
    """
    reps = required_reps(size, target_gb)
    gen_kwargs = dict(size=size, method=method, chunk_size=size)
    if threads is not None and method == "parallel":
        gen_kwargs["max_threads"] = threads

    buf = bytearray(size)

    # warm-up
    gen = dgen_py.Generator(**gen_kwargs)
    gen.fill_chunk(buf)

    j0 = _read_cpu_jiffies()
    t0 = time.perf_counter()
    for _ in range(reps):
        gen = dgen_py.Generator(**gen_kwargs)
        gen.fill_chunk(buf)
    elapsed = time.perf_counter() - t0
    cpu_s = cpu_seconds_elapsed(j0, _read_cpu_jiffies())

    total_gb = size * reps / 1e9
    return {
        "gb_s":    total_gb / elapsed,
        "cpu_eff": total_gb / cpu_s if cpu_s > 0 else float("inf"),
        "cpu_s":   cpu_s,
        "elapsed": elapsed,
        "reps":    reps,
        "total_gb": total_gb,
    }


# ── print helpers ──────────────────────────────────────────────────────────────

def human_size(n: int) -> str:
    for unit, div in [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10)]:
        if n >= div:
            v = n / div
            return f"{v:.0f} {unit}" if v == int(v) else f"{v:.1f} {unit}"
    return f"{n} B"


def print_section(title: str):
    print()
    print("─" * 72)
    print(f"  {title}")
    print("─" * 72)


def print_row(label: str, r: dict, cpu_cores_used: float | None = None):
    cpu_note = ""
    if cpu_cores_used is not None:
        cpu_note = f"  ({cpu_cores_used:.1f} cores equiv)"
    print(
        f"  {label:<32s}  {fmt_bw(r['gb_s'])}  {fmt_cpu(r['cpu_eff'])}{cpu_note}"
    )


def cores_equiv(r: dict) -> float:
    """Approximate number of cores consumed = cpu_s / wall_s."""
    if r["elapsed"] > 0:
        return r["cpu_s"] / r["elapsed"]
    return 0.0


# ── main ──────────────────────────────────────────────────────────────────────

SIZES = [
    64   * 1024,           #  64 KB
    512  * 1024,           # 512 KB
    1    * 1024 * 1024,    #   1 MB
    4    * 1024 * 1024,    #   4 MB
    16   * 1024 * 1024,    #  16 MB
    64   * 1024 * 1024,    #  64 MB
    256  * 1024 * 1024,    # 256 MB
]

ALL_CORES = multiprocessing.cpu_count()
THREAD_COUNTS = sorted({1, 4, ALL_CORES})   # de-dup if ALL_CORES < 4


def run_generate_buffer_section(target_gb: float):
    print_section("generate_buffer()  —  method comparison")
    print(
        f"  {'Label':<32s}  {'Throughput':>10s}  {'CPU efficiency':>15s}  cores"
    )
    print(f"  {'-'*32}  {'-'*10}  {'-'*15}  -----")

    for size in SIZES:
        sz = human_size(size)
        # XorStream — always single-threaded
        r = bench_generate_buffer(size, "xor", target_gb=target_gb)
        print_row(f"xor    {sz}", r, cores_equiv(r))

        # Parallel — test several thread counts
        for tc in THREAD_COUNTS:
            label = f"parallel {sz}  t={tc:2d}"
            if tc == ALL_CORES:
                label = f"parallel {sz}  t=all({ALL_CORES})"
            r = bench_generate_buffer(size, "parallel", threads=tc,
                                       target_gb=target_gb)
            print_row(label, r, cores_equiv(r))

        print()  # blank line between sizes


def run_streaming_section(target_gb: float):
    print_section("Generator.fill_chunk()  —  method comparison")
    print(
        f"  {'Label':<32s}  {'Throughput':>10s}  {'CPU efficiency':>15s}  cores"
    )
    print(f"  {'-'*32}  {'-'*10}  {'-'*15}  -----")

    for size in SIZES:
        sz = human_size(size)
        # XorStream
        r = bench_streaming(size, "xor", target_gb=target_gb)
        print_row(f"xor    {sz}", r, cores_equiv(r))

        # Parallel
        for tc in THREAD_COUNTS:
            label = f"parallel {sz}  t={tc:2d}"
            if tc == ALL_CORES:
                label = f"parallel {sz}  t=all({ALL_CORES})"
            r = bench_streaming(size, "parallel", threads=tc,
                                 target_gb=target_gb)
            print_row(label, r, cores_equiv(r))

        print()


def run_head_to_head(target_gb: float):
    """Side-by-side table for the most common object sizes at all-cores."""
    print_section(
        f"Head-to-head summary  —  parallel(t={ALL_CORES}) vs xor"
    )
    col = 12
    sizes_hh = [64*1024, 1*1024*1024, 4*1024*1024, 64*1024*1024, 256*1024*1024]
    header = (
        f"  {'Size':<10}  "
        f"{'par GB/s':>{col}}  {'par cores':>9}  "
        f"{'xor GB/s':>{col}}  {'xor cores':>9}  "
        f"{'xor/par':>7}"
    )
    print(header)
    print("  " + "-" * (len(header) - 2))

    for size in sizes_hh:
        sz = human_size(size)
        rp = bench_generate_buffer(size, "parallel", threads=ALL_CORES,
                                    target_gb=target_gb)
        rx = bench_generate_buffer(size, "xor", target_gb=target_gb)
        ratio = rx["gb_s"] / rp["gb_s"] if rp["gb_s"] > 0 else 0
        print(
            f"  {sz:<10}  "
            f"{rp['gb_s']:>{col}.2f}  {cores_equiv(rp):>9.1f}  "
            f"{rx['gb_s']:>{col}.2f}  {cores_equiv(rx):>9.1f}  "
            f"{ratio:>7.2f}x"
        )


def main():
    parser = argparse.ArgumentParser(
        description="Benchmark xor vs parallel data generation methods"
    )
    parser.add_argument(
        "--size-gb", type=float, default=2.0,
        help="GB of data to generate per benchmark cell (default: 2)"
    )
    parser.add_argument(
        "--no-streaming", action="store_true",
        help="Skip the streaming fill_chunk section"
    )
    args = parser.parse_args()

    print("=" * 72)
    print("  dgen-py  XorStream vs Parallel  —  Performance Benchmark")
    print("=" * 72)
    print(f"  dgen-py version : {dgen_py.__version__}")
    print(f"  CPU cores       : {ALL_CORES}  ({multiprocessing.cpu_count()} logical)")
    info = dgen_py.get_system_info() if hasattr(dgen_py, "get_system_info") else None
    if info:
        print(f"  NUMA nodes      : {info.get('num_nodes', '?')}")
    print(f"  Target GB/cell  : {args.size_gb}")
    print(f"  Sizes under test: {', '.join(human_size(s) for s in SIZES)}")
    print(f"  Thread counts   : {THREAD_COUNTS}  (parallel only)")
    print()
    print("  Columns:")
    print("    Throughput    — wall-clock GB/s")
    print("    CPU efficiency— GB generated per CPU-second (higher = less CPU)")
    print("    cores         — approx CPU cores consumed (cpu_time / wall_time)")

    run_generate_buffer_section(args.size_gb)

    if not args.no_streaming:
        run_streaming_section(args.size_gb)

    run_head_to_head(args.size_gb)

    print()
    print("=" * 72)
    print("  Done.")
    print("=" * 72)


if __name__ == "__main__":
    main()
