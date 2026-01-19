#!/usr/bin/env python3
# Benchmark dgen-py vs NumPy vs Numba - High-Performance Comparison
# Focus on fastest methods only

import dgen_py
import numpy as np
import time
import os
from concurrent.futures import ThreadPoolExecutor
try:
    from numba import njit, prange
    HAS_NUMBA = True
except ImportError:
    HAS_NUMBA = False
    print("WARNING: Numba not installed. Install with: pip install numba")
    print()

# Print out NUMA information that dgen-py sees
info = dgen_py.get_system_info()
if info:
    print(f"System Configuration:")
    print(f"  NUMA nodes: {info['num_nodes']}")
    print(f"  Physical cores: {info['physical_cores']}")
    print(f"  Deployment: {info['deployment_type']}")
    print()


# Test sizes: Only dgen-py can stream, others need full buffer in RAM
TEST_SIZE_SMALL = 10 * 1024 * 1024 * 1024   # 10 GB for NumPy, os.urandom (memory-limited)
TEST_SIZE_STREAMING = 100 * 1024 * 1024 * 1024   # 100 GB for Numba, dgen-py (streaming mode)

ITERATIONS_BASELINE = 3  # NumPy and os.urandom: 3 runs
ITERATIONS_STREAMING = 5  # Numba and dgen-py: 5 runs (streaming methods)

print("=" * 80)
print(f"RANDOM DATA GENERATION BENCHMARK")
print(f"  NumPy Multi-Thread: {ITERATIONS_BASELINE} runs of {TEST_SIZE_SMALL / (1024**3):.0f} GB each (full buffer in RAM)")
print(f"  Numba JIT: {ITERATIONS_STREAMING} runs of {TEST_SIZE_STREAMING / (1024**3):.0f} GB each (STREAMING - only 32 MB in RAM)")
print(f"  os.urandom(): {ITERATIONS_BASELINE} runs of {TEST_SIZE_SMALL / (1024**3):.0f} GB each (baseline)")
print(f"  dgen-py: {ITERATIONS_STREAMING} runs of {TEST_SIZE_STREAMING / (1024**3):.0f} GB each (STREAMING - only 32 MB in RAM)")
print("=" * 80)
print()

WARMUP_SIZE = 1 * 1024 * 1024 * 1024  # 1 GB warmup for all methods
WARMUP_CHUNK = 32 * 1024 * 1024  # 32 MB

# ============================================================================
# Numba JIT-compiled Xoshiro256++ RNG (if available)
# ============================================================================
if HAS_NUMBA:
    @njit(parallel=True)
    def xoshiro256pp_fill_chunk(buffer, state):
        """JIT-compiled Xoshiro256++ RNG - fills buffer in-place, returns updated state"""
        # Generate random uint64 values in parallel
        num_values = len(buffer) // 8
        for i in prange(num_values):
            # Xoshiro256++ algorithm
            result = state[0] + state[3]
            result = ((result << np.uint64(23)) | (result >> np.uint64(41))) + state[0]
            
            t = state[1] << np.uint64(17)
            state[2] ^= state[0]
            state[3] ^= state[1]
            state[1] ^= state[2]
            state[0] ^= state[3]
            state[2] ^= t
            state[3] = (state[3] << np.uint64(45)) | (state[3] >> np.uint64(19))
            
            # Write 8 bytes
            idx = i * 8
            for j in range(8):
                buffer[idx + j] = (result >> (j * 8)) & 0xFF
        
        return state
    
    def init_xoshiro_state(seed):
        """Initialize Xoshiro256++ state from seed"""
        state = np.zeros(4, dtype=np.uint64)
        # Do arithmetic as Python int (arbitrary precision), then mask to uint64
        GOLDEN = 0x9e3779b97f4a7c15
        MASK64 = 0xFFFFFFFFFFFFFFFF
        state[0] = np.uint64(seed & MASK64)
        state[1] = np.uint64((seed + GOLDEN) & MASK64)
        state[2] = np.uint64((seed + GOLDEN * 2) & MASK64)
        state[3] = np.uint64((seed + GOLDEN * 3) & MASK64)
        return state
    
    print("✓ Numba JIT compilation enabled (STREAMING MODE)")
    print()

# ============================================================================
# METHOD 1: NumPy Multi-Thread (all cores) - Best NumPy parallel option
# ============================================================================
print(f"METHOD 1: NumPy Multi-Thread [{info['physical_cores']} threads] - Parallel")
print("-" * 80)

# Warmup NumPy Multi-Thread with 1 GB
print("Warming up with 1 GB throwaway run...")
def warmup_numpy_chunk(args):
    chunk_size, seed = args
    rng = np.random.Generator(np.random.PCG64(seed))
    return rng.bytes(chunk_size)

work_items = [(WARMUP_CHUNK, j) for j in range(WARMUP_SIZE // WARMUP_CHUNK)]
with ThreadPoolExecutor(max_workers=info['physical_cores']) as executor:
    _ = list(executor.map(warmup_numpy_chunk, work_items))
print("Warmup complete. Starting benchmark...")
print()

def generate_numpy_chunk_threaded(args):
    """Generate random bytes using NumPy in a thread"""
    chunk_size, seed = args
    rng = np.random.Generator(np.random.PCG64(seed))
    return rng.bytes(chunk_size)

run_times_numpy_mt = []
num_workers = info['physical_cores']
CHUNK_SIZE = 32 * 1024 * 1024  # 32 MB chunks
chunks_per_worker = (TEST_SIZE_SMALL // CHUNK_SIZE) // num_workers

# Create thread pool once and reuse for all iterations
with ThreadPoolExecutor(max_workers=num_workers) as executor:
    for i in range(1, ITERATIONS_BASELINE + 1):
        start_time = time.perf_counter()
        
        # Create work items
        work_items = [(CHUNK_SIZE, i * 1000 + j) for j in range(num_workers * chunks_per_worker)]
        
        results = list(executor.map(generate_numpy_chunk_threaded, work_items))
        
        end_time = time.perf_counter()
        duration = end_time - start_time
        bytes_generated = sum(len(r) for r in results)
        throughput = (bytes_generated / (1024**3)) / duration
        
        run_times_numpy_mt.append(duration)
        print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GB/s")

avg_numpy_mt = sum(run_times_numpy_mt) / ITERATIONS_BASELINE
throughput_numpy_mt = (TEST_SIZE_SMALL / (1024**3)) / avg_numpy_mt
print(f"AVERAGE: {avg_numpy_mt:.4f} seconds | {throughput_numpy_mt:.2f} GB/s")
print()

# ============================================================================
# METHOD 2: Numba JIT Xoshiro256++ (if available) - Compiled parallel STREAMING
# ============================================================================
if HAS_NUMBA:
    print(f"METHOD 2: Numba JIT Xoshiro256++ [JIT-compiled, parallel, STREAMING]")
    print("-" * 80)
    
    # Warmup Numba with 1 GB streaming
    print("Warming up with 1 GB throwaway run (streaming)...")
    CHUNK_SIZE_NUMBA = 32 * 1024 * 1024  # 32 MB chunks
    buffer_numba = np.empty(CHUNK_SIZE_NUMBA, dtype=np.uint8)
    state = init_xoshiro_state(99999)
    bytes_generated = 0
    while bytes_generated < WARMUP_SIZE:
        state = xoshiro256pp_fill_chunk(buffer_numba, state)
        bytes_generated += CHUNK_SIZE_NUMBA
    print("Warmup complete. Starting benchmark...")
    print(f"Generating {TEST_SIZE_STREAMING / (1024**3):.0f} GB per run (streaming mode)")
    print()
    
    run_times_numba = []
    
    for i in range(1, ITERATIONS_STREAMING + 1):
        state = init_xoshiro_state(i * 54321)
        bytes_generated = 0
        
        start_time = time.perf_counter()
        
        # Stream through data in 32 MB chunks (like dgen-py!)
        while bytes_generated < TEST_SIZE_STREAMING:
            state = xoshiro256pp_fill_chunk(buffer_numba, state)
            bytes_generated += CHUNK_SIZE_NUMBA
        
        end_time = time.perf_counter()
        duration = end_time - start_time
        throughput = (TEST_SIZE_STREAMING / (1024**3)) / duration
        
        run_times_numba.append(duration)
        print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GB/s")
    
    avg_numba = sum(run_times_numba) / ITERATIONS_STREAMING
    throughput_numba = (TEST_SIZE_STREAMING / (1024**3)) / avg_numba
    print(f"AVERAGE: {avg_numba:.4f} seconds | {throughput_numba:.2f} GB/s")
    print()

# ============================================================================
# METHOD 3: os.urandom() - System random baseline [Single-threaded]
# ============================================================================
print("METHOD 3: os.urandom() [System random baseline - 10 GB only]")
print("-" * 80)

# Warmup os.urandom with 1 GB
print("Warming up with 1 GB throwaway run...")
bytes_generated = 0
while bytes_generated < WARMUP_SIZE:
    chunk_size = min(WARMUP_CHUNK, WARMUP_SIZE - bytes_generated)
    _ = os.urandom(chunk_size)
    bytes_generated += chunk_size
print("Warmup complete. Starting benchmark...")
print()

run_times_urandom = []
CHUNK_SIZE = 32 * 1024 * 1024  # 32 MB chunks

for i in range(1, ITERATIONS_BASELINE + 1):
    start_time = time.perf_counter()
    
    bytes_generated = 0
    while bytes_generated < TEST_SIZE_SMALL:
        chunk_size = min(CHUNK_SIZE, TEST_SIZE_SMALL - bytes_generated)
        data = os.urandom(chunk_size)
        bytes_generated += len(data)
    
    end_time = time.perf_counter()
    duration = end_time - start_time
    throughput = (TEST_SIZE_SMALL / (1024**3)) / duration
    
    run_times_urandom.append(duration)
    print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GB/s")

avg_urandom = sum(run_times_urandom) / ITERATIONS_BASELINE
throughput_urandom = (TEST_SIZE_SMALL / (1024**3)) / avg_urandom
print(f"AVERAGE: {avg_urandom:.4f} seconds | {throughput_urandom:.2f} GB/s")
print()

# ============================================================================
# METHOD 4: dgen-py (32 MB chunks) - NUMA-optimized Rust
# ============================================================================
print("METHOD 4: dgen-py (32 MB chunks) [NUMA-optimized Rust]")
print("-" * 80)

# Warmup dgen-py with 1 GB
print("Warming up with 1 GB throwaway run...")
gen = dgen_py.Generator(
    size=WARMUP_SIZE,
    dedup_ratio=1.0,
    compress_ratio=1.0,
    numa_mode="auto",
    max_threads=None,
    chunk_size=32 * 1024 * 1024
)
buffer = bytearray(gen.chunk_size)
while not gen.is_complete():
    nbytes = gen.fill_chunk(buffer)
    if nbytes == 0:
        break
print("Warmup complete. Starting benchmark...")
print(f"Generating {TEST_SIZE_STREAMING / (1024**3):.0f} GB per run (streaming mode)")
print()

run_times_dgen = []

for i in range(1, ITERATIONS_STREAMING + 1):
    # Create new generator for each run (reuses thread pool internally)
    gen = dgen_py.Generator(
        size=TEST_SIZE_STREAMING,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=None,
        chunk_size=32 * 1024 * 1024
    )
    
    buffer = bytearray(gen.chunk_size)
    
    start_time = time.perf_counter()
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buffer)
        if nbytes == 0:
            break
    
    end_time = time.perf_counter()
    duration = end_time - start_time
    throughput = (TEST_SIZE_STREAMING / (1024**3)) / duration
    
    run_times_dgen.append(duration)
    print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GB/s")

avg_duration_dgen = sum(run_times_dgen) / ITERATIONS_STREAMING
throughput_dgen = (TEST_SIZE_STREAMING / (1024**3)) / avg_duration_dgen
print(f"AVERAGE: {avg_duration_dgen:.4f} seconds | {throughput_dgen:.2f} GB/s")
print()

# ============================================================================
# FINAL COMPARISON
# ============================================================================
print("=" * 80)
print("PERFORMANCE COMPARISON SUMMARY")
print("=" * 80)
print(f"{'Method':<55} {'Throughput':>15} {'Speedup':>12}")
print("-" * 80)

results = [
    ("os.urandom() (baseline)", throughput_urandom, 1.0),
    ("NumPy Multi-Thread (best NumPy)", throughput_numpy_mt, throughput_numpy_mt / throughput_urandom),
]

if HAS_NUMBA:
    results.append(("Numba JIT Xoshiro256++ (compiled)", throughput_numba, throughput_numba / throughput_urandom))

results.append(("dgen-py 32 MB (NUMA-optimized Rust)", throughput_dgen, throughput_dgen / throughput_urandom))

for method, throughput, speedup in results:
    print(f"{method:<55} {throughput:>12.2f} GB/s {speedup:>11.1f}x")

print()
print("=" * 80)
print("KEY INSIGHTS:")
print("=" * 80)
print(f"  • System baseline (os.urandom, 10 GB): {throughput_urandom:.2f} GB/s")
print(f"  • NumPy Multi-Thread (10 GB - requires 10 GB RAM): {throughput_numpy_mt:.2f} GB/s")
if HAS_NUMBA:
    print(f"  • Numba JIT (100 GB - only 32 MB RAM via STREAMING): {throughput_numba:.2f} GB/s")
print(f"  • dgen-py (100 GB - only 32 MB RAM via STREAMING): {throughput_dgen:.2f} GB/s")
print()
print(f"  • dgen-py vs NumPy Multi-Thread: {throughput_dgen / throughput_numpy_mt:.1f}x faster")
if HAS_NUMBA:
    print(f"  • dgen-py vs Numba JIT: {throughput_dgen / throughput_numba:.1f}x faster")
print()
print("CRITICAL ARCHITECTURAL ADVANTAGE:")
print("  • NumPy: Must hold ENTIRE dataset in RAM (10 GB memory required)")
print("  • Numba & dgen-py: TRUE STREAMING - only 32 MB in RAM regardless of dataset size")
print("  • Both can generate UNLIMITED data (TB+) on systems with <1 GB free RAM")
print()
print("OTHER ADVANTAGES:")
print(f"  • NUMA-aware: Optimized memory allocation per node")
print(f"  • Thread pool reuse: Created once, reused across operations")
print(f"  • Rust implementation: Memory-safe, no GIL limitations")
print(f"  • Per-core throughput: {throughput_dgen / info['physical_cores']:.2f} GB/s")

