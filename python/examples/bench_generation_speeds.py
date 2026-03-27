#!/usr/bin/env python3
"""
dgen-py Generation Speed Benchmark
====================================
Measures the throughput of every major dgen-py data-generation API so you can
choose the right one for your workload.

APIs tested
-----------
1. fill_chunk (streaming)   — single reusable buffer, constant 32 MB memory
2. fill_chunk (large chunk) — 64 MB + 256 MB chunks (more parallelism per chunk)
3. generate_buffer          — in-process BytesView (what kv-cache benchmark uses)
4. create_bytearrays        — bulk pre-allocation then fill (pre-generation pattern)
5. streaming to files       — generate N x M GB files with constant memory footprint

Thread-count scaling
--------------------
Tests 1, 4, 8 and all-cores to show scaling curve.

Compress ratio impact
---------------------
compress_ratio=1.0 (incompressible) vs 2.0 (1.3-1.5x faster per docs).

Memory footprint
----------------
Streaming keeps RAM usage pinned at one chunk (32-64 MB) regardless of total
dataset size -- that is the whole point.  Section 6 demonstrates this by
writing multiple multi-GB files while holding only a single 64 MB buffer.

Usage
-----
    python bench_generation_speeds.py [--size-gb N] [--out-dir DIR]
                                       [--file-gb F] [--num-files N]

    --size-gb N    GB per memory-only measurement (default: 4)
    --out-dir DIR  directory for file-streaming test (default: /mnt/nvme_data/dgen_bench)
                   set to '' to skip the file-streaming section
    --file-gb F    size of each output file in GB (default: 8)
    --num-files N  number of files to write (default: 4)
"""

import argparse
import sys
import time
import os
import shutil

try:
    import dgen_py
except ImportError:
    print("ERROR: dgen-py is not installed.  Run: pip install dgen-py")
    sys.exit(1)

# ── helpers ───────────────────────────────────────────────────────────────────

def fmt_bw(gb_per_s: float) -> str:
    return f"{gb_per_s:7.2f} GB/s"

def measure_fill_chunk(size_bytes: int, chunk_size: int,
                       max_threads=None, compress_ratio=1.0,
                       warmup=True) -> float:
    """Return throughput in GB/s for streaming fill_chunk."""
    gen = dgen_py.Generator(
        size=size_bytes,
        compress_ratio=compress_ratio,
        numa_mode="auto",
        max_threads=max_threads,
        chunk_size=chunk_size,
    )
    buf = bytearray(chunk_size)
    if warmup:
        gen.fill_chunk(buf)   # warm JIT / thread pool
        gen.reset()

    start = time.perf_counter()
    while not gen.is_complete():
        if gen.fill_chunk(buf) == 0:
            break
    elapsed = time.perf_counter() - start
    return (size_bytes / 1e9) / elapsed


def measure_generate_buffer(size_bytes: int, repeats: int = 8) -> float:
    """Return throughput in GB/s for dgen_py.generate_buffer(size)."""
    # warm up
    dgen_py.generate_buffer(size_bytes)

    start = time.perf_counter()
    for _ in range(repeats):
        _ = dgen_py.generate_buffer(size_bytes)
    elapsed = time.perf_counter() - start
    return (size_bytes * repeats / 1e9) / elapsed


def measure_create_bytearrays(total_bytes: int, chunk_size: int,
                               max_threads=None) -> tuple:
    """
    Return (alloc_gb_s, fill_gb_s) for create_bytearrays + fill_chunk.
    """
    num_chunks = total_bytes // chunk_size

    # Allocation
    t0 = time.perf_counter()
    chunks = dgen_py.create_bytearrays(count=num_chunks, size=chunk_size)
    alloc_time = time.perf_counter() - t0
    alloc_gb_s = (total_bytes / 1e9) / alloc_time

    # Fill
    gen = dgen_py.Generator(
        size=total_bytes,
        numa_mode="auto",
        max_threads=max_threads,
        chunk_size=chunk_size,
    )
    t0 = time.perf_counter()
    for buf in chunks:
        gen.fill_chunk(buf)
    fill_time = time.perf_counter() - t0
    fill_gb_s = (total_bytes / 1e9) / fill_time

    del chunks
    return alloc_gb_s, fill_gb_s


def measure_stream_to_file(path: str, size_bytes: int, chunk_size: int,
                           compress_ratio: float = 1.0) -> dict:
    """
    Generate size_bytes with streaming fill_chunk and write every chunk
    directly to path using a single reusable buffer (constant RAM).

    Returns dict with keys: gen_gb_s, write_gb_s, total_gb_s, file_gb.
    """
    buf = bytearray(chunk_size)
    gen = dgen_py.Generator(
        size=size_bytes,
        compress_ratio=compress_ratio,
        numa_mode="auto",
        chunk_size=chunk_size,
    )

    bytes_written = 0
    gen_ns = 0
    write_ns = 0

    with open(path, "wb", buffering=0) as f:   # unbuffered: no libc double-buffer
        while not gen.is_complete():
            t0 = time.perf_counter_ns()
            n  = gen.fill_chunk(buf)
            gen_ns += time.perf_counter_ns() - t0
            if n == 0:
                break

            view = memoryview(buf)[:n]           # zero-copy slice
            t0   = time.perf_counter_ns()
            f.write(view)
            write_ns += time.perf_counter_ns() - t0
            bytes_written += n

    total_ns = gen_ns + write_ns
    gb = bytes_written / 1e9
    return {
        "file_gb":    gb,
        "gen_gb_s":   gb / (gen_ns   / 1e9) if gen_ns   else 0.0,
        "write_gb_s": gb / (write_ns  / 1e9) if write_ns  else 0.0,
        "total_gb_s": gb / (total_ns  / 1e9) if total_ns  else 0.0,
    }


def section(title: str):
    print()
    print("─" * 70)
    print(f"  {title}")
    print("─" * 70)


# ── main ─────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--size-gb",   type=float, default=4,
                        help="GB per memory-only measurement (default: 4)")
    parser.add_argument("--out-dir",   type=str,
                        default="/mnt/nvme_data/dgen_bench",
                        help="directory for file-streaming test; set to '' to skip")
    parser.add_argument("--file-gb",   type=float, default=8,
                        help="size of each output file in GB (default: 8)")
    parser.add_argument("--num-files", type=int,   default=4,
                        help="number of files to write (default: 4)")
    args = parser.parse_args()

    size = int(args.size_gb * 1e9)

    info = dgen_py.get_system_info()
    ncores = info["logical_cpus"] if info else os.cpu_count()
    phys   = info["physical_cores"] if info else ncores
    nodes  = info["num_nodes"] if info else 1
    deploy = info["deployment_type"] if info else "unknown"

    print()
    print("=" * 70)
    print("  dgen-py Generation Speed Benchmark")
    print("=" * 70)
    print(f"  CPU:           {ncores} logical CPUs, {phys} physical cores")
    print(f"  NUMA:          {nodes} node(s) — {deploy}")
    print(f"  Test size:     {args.size_gb:.1f} GB per measurement")
    print(f"  dgen-py ver:   {getattr(dgen_py, '__version__', 'n/a')}")
    file_section = bool(args.out_dir)
    if file_section:
        total_file_gb = args.num_files * args.file_gb
        print(f"  File test:     {args.num_files} x {args.file_gb:.1f} GB = {total_file_gb:.1f} GB  ->  {args.out_dir}")
    print("=" * 70)

    thread_counts = sorted(set([1, min(4, ncores), min(8, ncores), ncores]))
    default_chunk = 32 * 1024 * 1024   # 32 MB

    # ── 1. Thread scaling (streaming, 32 MB chunk)  ───────────────────────────
    section("1. Thread-count scaling  (fill_chunk, 32 MB chunk, compress=1.0)")
    print(f"  {'Threads':>8}  {'Throughput':>12}  {'Per-core':>12}")
    print(f"  {'-------':>8}  {'----------':>12}  {'--------':>12}")
    for t in thread_counts:
        bw = measure_fill_chunk(size, default_chunk, max_threads=t)
        per_core = bw / t
        print(f"  {t:>8}  {fmt_bw(bw)}  {fmt_bw(per_core)}")

    # ── 2. Chunk size impact (all cores)  ─────────────────────────────────────
    section("2. Chunk size impact  (fill_chunk, all cores, compress=1.0)")
    chunk_sizes = [
        (  8, "8 MB  (min parallel)"),
        ( 32, "32 MB (default)     "),
        ( 64, "64 MB               "),
        (256, "256 MB              "),
    ]
    print(f"  {'Chunk size':>22}  {'Throughput':>12}")
    print(f"  {'----------':>22}  {'----------':>12}")
    for mb, label in chunk_sizes:
        cs = mb * 1024 * 1024
        bw = measure_fill_chunk(size, cs)
        print(f"  {label}  {fmt_bw(bw)}")

    # ── 3. compress_ratio impact (all cores, 32 MB chunk)  ────────────────────
    section("3. compress_ratio impact  (fill_chunk, all cores, 32 MB chunk)")
    print(f"  {'compress_ratio':>16}  {'Throughput':>12}  {'Notes'}")
    print(f"  {'----------':>16}  {'----------':>12}  {'-----'}")
    for cr, note in [(1.0, "incompressible (default)"),
                     (2.0, "2:1 compressible        "),
                     (3.0, "3:1 compressible        ")]:
        bw = measure_fill_chunk(size, default_chunk, compress_ratio=cr)
        print(f"  {cr:>16.1f}  {fmt_bw(bw)}  {note}")

    # ── 4. generate_buffer  (in-process BytesView, used by kv-cache bench)  ──
    section("4. generate_buffer()  (BytesView, zero-copy return)")
    entry_sizes = [
        ( 64, " 64 MB  — typical small KV entry"),
        (256, "256 MB  — medium KV entry       "),
        (512, "512 MB  — large KV entry        "),
    ]
    print(f"  {'Entry size':>32}  {'Throughput':>12}  {'Latency':>10}")
    print(f"  {'----------':>32}  {'----------':>12}  {'-------':>10}")
    for mb, label in entry_sizes:
        entry_bytes = mb * 1024 * 1024
        # generate_buffer uses all cores internally
        bw = measure_generate_buffer(entry_bytes, repeats=max(4, size // entry_bytes))
        lat_ms = (entry_bytes / 1e9) / bw * 1000
        print(f"  {label}  {fmt_bw(bw)}  {lat_ms:8.1f} ms")

    # ── 5. create_bytearrays + fill  (pre-generation pattern)  ───────────────
    section("5. create_bytearrays() + fill_chunk  (pre-generation pattern)")
    print(f"  {'Chunk size':>10}  {'Alloc':>12}  {'Fill':>12}  {'Notes'}")
    print(f"  {'----------':>10}  {'-----':>12}  {'----':>12}  {'-----'}")
    for mb, note in [(32,  "default chunk"), (64, "large chunk  ")]:
        cs = mb * 1024 * 1024
        alloc_bw, fill_bw = measure_create_bytearrays(size, cs)
        print(f"  {mb:>8} MB  {fmt_bw(alloc_bw)}  {fmt_bw(fill_bw)}  {note}")

    # -- 6. Streaming to files  ------------------------------------------------
    file_bw_results = []   # list of (total_gb_s, gen_gb_s, write_gb_s)

    if file_section:
        section(
            f"6. Streaming fill_chunk -> files  "
            f"({args.num_files} x {args.file_gb:.1f} GB, 64 MB chunk, compress=1.0)"
        )
        print(f"  Output dir    : {args.out_dir}")
        print(f"  RAM footprint : ONE 64 MB buffer — constant regardless of dataset size")
        print()

        os.makedirs(args.out_dir, exist_ok=True)
        file_bytes = int(args.file_gb * 1e9)
        file_chunk = 64 * 1024 * 1024   # 64 MB sweet-spot from section 2

        print(f"  {'File':>6}  {'Size':>8}  {'Gen rate':>12}  {'Write rate':>12}  {'Total (gen+write)':>18}")
        print(f"  {'----':>6}  {'----':>8}  {'--------':>12}  {'----------':>12}  {'------------------':>18}")

        wall_start = time.perf_counter()
        for i in range(1, args.num_files + 1):
            fpath = os.path.join(args.out_dir, f"kvcache_{i:03d}.bin")
            r = measure_stream_to_file(fpath, file_bytes, file_chunk)
            file_bw_results.append((r["total_gb_s"], r["gen_gb_s"], r["write_gb_s"]))
            print(f"  {i:>6}  {r['file_gb']:>6.2f} GB"
                  f"  {fmt_bw(r['gen_gb_s'])}"
                  f"  {fmt_bw(r['write_gb_s'])}"
                  f"  {fmt_bw(r['total_gb_s'])}")
        wall_elapsed = time.perf_counter() - wall_start
        total_gb = args.num_files * args.file_gb

        print(f"  {'------':>6}  {'--------':>8}  {'------------':>12}  {'------------':>12}  {'------------------':>18}")
        print(f"  {'TOTAL':>6}  {total_gb:>6.1f} GB"
              f"  {'':>12}  {'':>12}  {fmt_bw(total_gb / wall_elapsed):>18}")
        print()
        print(f"  Wall time for all {args.num_files} files : {wall_elapsed:.1f} s")
        print(f"  RAM used for generation      : 64 MB  (one reusable chunk buffer)")

        shutil.rmtree(args.out_dir, ignore_errors=True)

    # -- Summary  --------------------------------------------------------------
    section("Summary")
    streaming_bw  = measure_fill_chunk(size, default_chunk)
    gen_buf_bw    = measure_generate_buffer(256 * 1024 * 1024, repeats=max(4, size // (256*1024*1024)))
    _, bulk_bw    = measure_create_bytearrays(size, default_chunk)

    print(f"  Streaming fill_chunk (32 MB, all cores) : {fmt_bw(streaming_bw)}")
    print(f"  generate_buffer (256 MB entry)          : {fmt_bw(gen_buf_bw)}")
    print(f"  create_bytearrays + fill (32 MB chunks) : {fmt_bw(bulk_bw)}")
    if file_bw_results:
        avg_total = sum(r[0] for r in file_bw_results) / len(file_bw_results)
        avg_write = sum(r[2] for r in file_bw_results) / len(file_bw_results)
        print(f"  Stream to file avg (gen+write, wall)    : {fmt_bw(avg_total)}")
        print(f"  Storage write throughput (avg)          : {fmt_bw(avg_write)}")
    print()
    print(f"  Per physical core (streaming)           : {fmt_bw(streaming_bw / phys)}")
    print()
    print("  API guidance:")
    print("    fill_chunk -> file  — streaming to storage; constant 64 MB RAM; any dataset size")
    print("    fill_chunk          — best for pure generation (constant 32 MB RAM)")
    print("    generate_buffer     — best for in-process single entries (kv-cache)")
    print("    create_bytearrays   — best when entire dataset must live in RAM")
    print()


if __name__ == "__main__":
    main()
