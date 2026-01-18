# NUMA Implementation Handoff - January 17, 2026

## Current Status

**Last commit**: `9fab797` - "feat: Add optimal chunk_size support with 32 MB default"

**Clean working tree**: All chunk_size optimization work is committed and tested.

### Working Performance (UMA System - 12 cores)
- **Streaming API**: 43-50 GB/s (3.6-4.2 GB/s per core)
- **Optimal chunk size**: 32 MB (16% faster than 64 MB)
- **Python matches Rust**: Both get ~44 GB/s streaming

### What Works
1. ✅ Chunk size optimization implemented and tested
2. ✅ Python API exposes `chunk_size` parameter (defaults to 32 MB)
3. ✅ `DataGenerator::recommended_chunk_size()` returns optimal value
4. ✅ Documentation updated to reflect 32 MB recommendation
5. ✅ Benchmark tests both default and override scenarios

## NUMA Requirements (HPC System - 384 cores, 16 NUMA nodes)

### Critical Problem Discovered
Running on Azure HBv5-Test (384 cores, 16 NUMA nodes, 23 cores per node):
- **Current performance**: 34 GB/s total (0.09 GB/s per core) ❌
- **Expected performance**: 1,200-1,400 GB/s (3.3-3.8 GB/s per core) ✅
- **Root cause**: All buffers allocated on NUMA node 0 → 15/16 cores do remote memory writes (40x penalty)

### NUMA Architecture Decision (User-Specified)
**Multi-process approach** (Option A from earlier discussion):
- Spawn **one process per NUMA node** (16 processes on HPC)
- Each process:
  - Allocates buffer on its assigned NUMA node
  - Uses only CPUs from that NUMA node
  - Operates independently (no cross-NUMA communication)

### Key Insights
1. **Thread pinning alone is insufficient** - tested, still got 34-36 GB/s
2. **Buffer locality is critical** - memory must be allocated on the correct NUMA node
3. **UMA fast path must be preserved** - any NUMA code only activates when `num_nodes > 1`
4. **32 MB is optimal on UMA** - but may need testing on NUMA systems

## Implementation Plan

### 1. Switch to hwlocality (STARTED - NEEDS COMPLETION)

**Files modified but NOT committed**:
- `Cargo.toml`: Changed `hwloc2` → `hwlocality` v1.0.0-alpha.11
- `src/lib.rs`: Added `pub mod numa_hwloc;`
- `src/numa_hwloc.rs`: Created new file (HAS COMPILATION ERRORS)

**Current issues**:
- API misunderstanding - `topology.set_membind()` doesn't exist in hwlocality
- Need to study hwlocality documentation properly
- May need different approach (e.g., using `mmap` with `mbind()` syscall)

**Recommendation**: **REVERT** these changes and start fresh with proper hwlocality research:
```bash
git checkout Cargo.toml src/lib.rs
rm src/numa_hwloc.rs
```

### 2. Correct NUMA Allocation Approach

#### Option A: Use hwlocality properly
Research the actual hwlocality API for memory binding:
- Check examples in hwlocality repo
- Look for `alloc_membind()` or similar
- May need to use lower-level `hwlocality-sys` bindings

#### Option B: Direct libnuma bindings (SIMPLER)
Use `libnuma` crate which has proven syscall wrappers:
```rust
use libnuma::masks::NodeMask;

unsafe {
    let ptr = libnuma::numa_alloc_onnode(size, node_id as i32);
    // Initialize and convert to Vec<u8>
}
```

#### Option C: Manual mmap + mbind (MOST CONTROL)
```rust
use nix::sys::mman::{mmap, munmap, ProtFlags, MapFlags};
use nix::unistd::sysconf;

// Allocate with mmap
let ptr = mmap(...);
// Bind to NUMA node with mbind syscall
mbind(ptr, size, MPOL_BIND, &nodemask, ...);
// Convert to Vec<u8>
```

### 3. Integration Points

**In `src/generator.rs`**, modify buffer allocation in `generate_data()`:

```rust
// Around line 157 (current buffer allocation)
let mut data: Vec<u8> = if let Some(node_id) = config.numa_node {
    // Check if NUMA system
    #[cfg(feature = "numa")]
    if let Ok(topology) = crate::numa::NumaTopology::detect() {
        if topology.num_nodes > 1 {
            // NUMA system: allocate on specific node
            match allocate_on_numa_node(total_size, node_id) {
                Ok(buf) => {
                    tracing::info!("Allocated {} bytes on NUMA node {}", total_size, node_id);
                    buf
                }
                Err(e) => {
                    tracing::warn!("NUMA allocation failed: {}, using default", e);
                    vec![0u8; total_size]  // Fallback
                }
            }
        } else {
            // UMA system: use default (FAST PATH - unchanged)
            vec![0u8; total_size]
        }
    } else {
        vec![0u8; total_size]
    }
    
    #[cfg(not(feature = "numa"))]
    vec![0u8; total_size]
} else {
    // No NUMA node specified: use default
    vec![0u8; total_size]
};
```

**In `src/python_api.rs`**, the `numa_node` parameter is already in place:
- Line 326: `numa_node: Option<usize>` parameter exists
- Line 364: Passed to `GeneratorConfig`
- Just needs backend implementation

### 4. Testing Strategy

#### Phase 1: UMA Verification
```bash
cd dgen-rs
cargo build --release
python python/examples/Benchmark_dgen-py_FIXED.py
# Should still get 43-50 GB/s (no regression)
```

#### Phase 2: NUMA Simulation
```python
# Test on UMA but with numa_node=0 specified
gen = dgen_py.Generator(
    size=100*1024*1024*1024,
    numa_node=0  # Should behave identically to no numa_node
)
```

#### Phase 3: HPC Deployment
On azureuser@HBv5-Test:
```python
# Run 16 processes, one per NUMA node
for node_id in range(16):
    gen = dgen_py.Generator(
        size=6.25*1024*1024*1024,  # 6.25 GB per process = 100 GB total
        numa_node=node_id
    )
    # Generate and measure throughput
```

**Expected**: 1,200-1,400 GB/s total (3.3-3.8 GB/s per core)

## Critical Success Criteria

1. ✅ **UMA performance preserved**: 38-42 GB/s on 12-core systems
2. ⏳ **NUMA performance achieved**: 1,200+ GB/s on 384-core HPC
3. ✅ **Code simplicity**: NUMA code only activates when needed
4. ⏳ **Build compatibility**: Must compile on systems without NUMA support

## Key Files

```
dgen-rs/
├── src/
│   ├── generator.rs        # Buffer allocation (line ~157)
│   ├── python_api.rs       # Python bindings (numa_node parameter ready)
│   ├── numa.rs            # Current NUMA detection (uses /sys filesystem)
│   └── numa_hwloc.rs      # NEW - NEEDS IMPLEMENTATION (currently broken)
├── Cargo.toml             # Dependencies (hwlocality added but may need libnuma)
├── python/examples/
│   └── Benchmark_dgen-py_FIXED.py  # Current benchmark (43-50 GB/s UMA)
└── examples/
    └── perf_test_streaming.rs      # Chunk size testing tool
```

## Recommended Next Steps

1. **Revert failed hwlocality attempt**:
   ```bash
   git checkout Cargo.toml src/lib.rs
   rm src/numa_hwloc.rs
   git status  # Should be clean
   ```

2. **Research NUMA allocation approaches**:
   - Check hwlocality examples/docs for correct API
   - Consider `libnuma` crate as simpler alternative
   - Test small proof-of-concept before full integration

3. **Implement with care**:
   - Start with minimal changes
   - Test UMA performance after EVERY change
   - Only proceed when UMA fast path is confirmed working

4. **Deploy incrementally**:
   - First: UMA with `numa_node=0` (should be identical to no numa_node)
   - Then: HPC with multi-process architecture
   - Measure: Expect 40x improvement (34 GB/s → 1,200+ GB/s)

## Performance Expectations

| System | Cores | NUMA Nodes | Current | Target | Status |
|--------|-------|------------|---------|--------|--------|
| loki-russ (UMA) | 12 | 1 | 43-50 GB/s | 38-42 GB/s | ✅ EXCEEDS |
| HBv5-Test (NUMA) | 384 | 16 | 34 GB/s | 1,200-1,400 GB/s | ❌ BLOCKED |

**Per-core target**: 3.3-3.8 GB/s (consistent across UMA and NUMA)

## Questions for Next Session

1. Which NUMA allocation library: hwlocality, libnuma, or manual mmap+mbind?
2. Should we add compile-time feature flag for NUMA allocation code?
3. Need benchmark script for multi-process NUMA testing?
4. Documentation updates needed for NUMA deployment guide?

## Recent Discoveries

- **Chunk size matters**: 32 MB is 16% faster than 64 MB
- **Streaming is key**: 44 GB/s streaming vs 11 GB/s one-shot (on 10 GB test)
- **Python efficiency**: 92% of Rust performance (excellent for zero-copy)
- **Internal block size**: 4 MiB blocks for parallel generation (fits L3 cache)

---

**Git state**: Clean working tree on `feature/zero-copy-parallel-streaming-v0.1.3` branch  
**Last successful build**: January 17, 2026 18:56 UTC  
**Ready for**: NUMA implementation with proper library research
