# Benchmark dgen-py performance

import dgen_py
import time

# Print out NUMA information that dgen-py sees
info = dgen_py.get_system_info()
if info:
    print(f"NUMA nodes: {info['num_nodes']}")
    print(f"Physical cores: {info['physical_cores']}")
    print(f"Deployment: {info['deployment_type']}")


# Constants for 1 TB and 256 MB chunks (for parallel generation)
ONE_TB = 1024 * 1024 * 1024 * 1024
CHUNK_SIZE = 256 * 1024 * 1024  # 256 MB = 64 blocks (optimal for parallelization)
ITERATIONS = 10

run_times = []

print(f"Starting Benchmark: 10 runs of 1 TB each ({CHUNK_SIZE / (1024**2):.0f} MB chunks)")
print("Using PARALLEL STREAMING (fill_chunk with large buffers)")
print("-" * 60)

for i in range(1, ITERATIONS + 1):
    # Initialize the generator for 1 TB
    # With max_threads=None and large chunks (256 MB), fill_chunk() will use parallel generation
    gen = dgen_py.Generator(
        size=ONE_TB,
        dedup_ratio=1.0,
        compress_ratio=1.0,
        numa_mode="auto",
        max_threads=None  # Use all available CPUs for parallel fill_chunk
    )

    # Pre-allocate the buffer to avoid allocation overhead inside the loop
    buf = bytearray(CHUNK_SIZE)
    
    start_time = time.perf_counter()
    
    # Streaming loop - fill_chunk() will parallelize each 256 MB chunk
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buf)
        if nbytes == 0:
            break
            
    end_time = time.perf_counter()
    duration = end_time - start_time
    throughput = (ONE_TB / (1024**3)) / duration  # GiB/s
    
    run_times.append(duration)
    print(f"Run {i:02d}: {duration:.4f} seconds | {throughput:.2f} GiB/s")

# Calculate Statistics
avg_duration = sum(run_times) / ITERATIONS
avg_throughput = (ONE_TB / (1024**3)) / avg_duration

print("-" * 60)
print(f"AVERAGE DURATION:   {avg_duration:.4f} seconds")
print(f"AVERAGE THROUGHPUT: {avg_throughput:.2f} GiB/s")
print(f"TOTAL DATA MOVED:   { (ONE_TB * ITERATIONS) / (1024**4):.1f} TB")
print()
print("NOTE: Using 256 MB chunks enables parallel generation in fill_chunk().")
print("Chunks >= 8 MB (2 blocks) use multi-threaded rayon generation.")
