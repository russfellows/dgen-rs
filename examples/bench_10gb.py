#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# 10-iteration per-object benchmark at 10 GB.
#
# Each iteration:
#   1. Print `free -h` to show available memory before generation.
#   2. Generate 10 GB via Generator(size=N).get_chunk(N) — new Generator
#      (and DataGenerator + Rayon thread pool) per call.  This is EXACTLY
#      how dlio_benchmark calls dgen-py for large tensors.
#   3. Record elapsed time.
#   4. Print the result.
#   5. Explicitly del the BytesView + gc.collect() to release the 10 GB buffer.
#   6. Print `free -h` to show memory returned to OS.
#
# Usage:
#   source .venv/bin/activate
#   python examples/bench_10gb.py

import gc
import subprocess
import sys
import time

try:
    import dgen_py
    from dgen_py import Generator
except ImportError:
    print("ERROR: dgen-py not installed.  Run:  source .venv/bin/activate")
    sys.exit(1)

SIZE = 10 * 1024 ** 3  # 10 GB
RUNS = 10


def free():
    result = subprocess.run(["free", "-h"], capture_output=True, text=True)
    print(result.stdout, end="")


def main():
    print(f"=== dgen-py 10 GB per-object benchmark  ({RUNS} runs) ===")
    print("  Calls Generator(size=10 GB).get_chunk(10 GB) — new Generator every call.")
    print("  Buffer explicitly del'd + gc.collect() after each timed call.")
    print()

    times = []

    for i in range(1, RUNS + 1):
        print(f"── Run {i}/{RUNS}  (before generation) ──")
        free()

        t0 = time.perf_counter()
        gen = Generator(size=SIZE)
        bv = gen.get_chunk(SIZE)
        elapsed = time.perf_counter() - t0

        actual_bytes = len(bv) if bv is not None else 0
        gb_s = actual_bytes / elapsed / 1e9
        print(f"  → {elapsed:.3f} s   {gb_s:.2f} GB/s   ({actual_bytes} bytes)")
        times.append(gb_s)

        # Explicitly release the 10 GB buffer before the next iteration.
        del bv
        del gen
        gc.collect()

        print(f"── Run {i}/{RUNS}  (after free) ──")
        free()
        print()

    avg = sum(times) / len(times)
    mn  = min(times)
    mx  = max(times)
    srt = sorted(times)
    med = srt[len(srt) // 2]

    print("=== Summary ===")
    print(f"  Runs:    {RUNS}")
    for idx, v in enumerate(times, 1):
        print(f"  Run {idx:2d}:  {v:.2f} GB/s")
    print()
    print(f"  Min:     {mn:.2f} GB/s")
    print(f"  Median:  {med:.2f} GB/s")
    print(f"  Avg:     {avg:.2f} GB/s")
    print(f"  Max:     {mx:.2f} GB/s")


if __name__ == "__main__":
    main()
