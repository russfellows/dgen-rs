# Design: Parallel Numeric Distributions (uniform floats + normalization)

Status: **reviewed, decisions locked — ready for implementation**.
Author: Claude Code (session with Russ Fellows), 2026-07-10.
Target version: 0.3.0 (first breaking-free additive release after 0.2.4).

Revision note: this is v2, incorporating Russ's inline review (2026-07-10).
The review notes are kept in place as a record of the decision; the
resolution is stated immediately after each. Net effect: keep the new API
low-level and dgen-rs-consistent (`BytesView`/buffer-protocol first, no
NumPy arrays returned in v1), narrower/more honest v1 surface (no
forward-declared unsupported options), and a tighter validation contract.

## 1. Motivation

While investigating [mlcommons/storage#625](https://github.com/mlcommons/storage/issues/625)
(VectorDB benchmark recall is low because vectors are uniformly random) and a
related question about whether `mlp-storage`'s VectorDB benchmark
(`vdb_benchmark/vdbbench/load_vdb.py::generate_vectors()`) could use dgen-py
instead of NumPy, we benchmarked the two head-to-head at the benchmark's real
parameters (dimension 512/1536, chunks of up to 1,000,000 vectors — see
`mlp-storage/vdb_benchmark/vdbbench/configs/*.yaml`).

`generate_vectors()`'s actual contract is:

```python
def generate_vectors(num_vectors, dim, distribution='uniform'):
    vectors = np.random.random((num_vectors, dim)).astype(np.float32)  # [0,1)
    norms = np.linalg.norm(vectors, axis=1, keepdims=True)
    vectors /= norms                                                   # L2-normalize rows
    return vectors
```

Two properties of dgen-py's *existing* API make it unsuitable as a drop-in
replacement today:

1. `generate_buffer()`/`Generator.get_chunk()` return raw random **bytes**,
   not floats with a defined distribution. Naively reinterpreting those bytes
   as `float32` is invalid: ~0.38% of individual float32 values come out NaN,
   and finite values span the full float32 dynamic range instead of a bounded
   range. For a 128-dim vector, that NaN rate means ~38% of vectors would come
   out entirely NaN after L2-normalization (NaN propagates through the norm).
2. There is no vector-normalization primitive at all.

Working around this in pure Python (bit-masking raw bytes into valid
`[0, 1)` floats, then `np.linalg.norm` + divide) proves the underlying
dgen-py generation engine is already dramatically faster than NumPy, but the
Python-side conversion completely erases that advantage. Measured on a
28-core sandbox, production chunk size (1,000,000 × 1536, 6.14 GB):

| Phase | 1 thread | 2+ threads | Scales with dgen thread count? |
|---|---:|---:|:---:|
| Raw generation (`Generator.get_chunk`) | 3.42 s (1.80 GB/s) | **0.27 s (~23 GB/s)** | Yes |
| Bit-mask → uniform float32 (Python/NumPy) | 5.35 s | 5.30 s | **No — fixed cost** |
| L2-normalize (Python/NumPy) | 4.14 s | 4.10 s | **No — fixed cost** |
| **Total** | 12.9 s | **9.65 s** | dominated by the two NumPy-side phases |

NumPy's own `generate_vectors()` (generation + normalize together) takes
**21.35 s** for the same size. So today, dgen-py's raw engine is ~80x faster
than NumPy at generation alone, but doing the conversion/normalize work in
Python only nets a ~2.2x end-to-end win, because ~9.4 s of fixed,
single-threaded Python-side cost swamps the ~80x gain.

**If the bit-mask and normalize steps move into Rust** (reusing the exact
`par_chunks_mut` engine `generate_buffer()` already uses), both should scale
with thread count the same way raw generation does. Extrapolating from the
raw-generation scaling factor (~13x at 2+ threads vs. 1 thread), total time
for this workload should drop from today's 9.65s to roughly **~1s** — an
end-to-end speedup over NumPy in the range of **15-20x**, not ~2x.

## 2. Goals

Russ (dgen-py's author) wants this to be more than a one-off VDB fix: dgen-py
should grow into a credible high-performance alternative to
`numpy.random`/`numpy.linalg` for the operations it's good at (bulk
generation, embarrassingly-parallel elementwise/row-wise math).

**Naming/shape principle (per review, §6)**: the new functions follow
dgen-py's *own* existing conventions — `generate_`-prefixed names, flat
namespace, `BytesView`/buffer-protocol return and input types — rather than
adopting NumPy's naming or API shape where the two diverge. NumPy analogies
below (e.g. "direct analog of `numpy.random.uniform()`") are for
orientation/documentation only — they explain what the function *does* to
a reader coming from NumPy, they do not drive naming or signature choices.

This design proposes **three new functions**, all additive (no changes to
existing API surface):

1. **`generate_uniform`** — general-purpose parallel uniform float32
   generation. Direct analog of `numpy.random.uniform()` /
   `numpy.random.random()`. Useful far beyond VDB (any caller that wants
   fast bulk random floats).
2. **`normalize_rows`** — general-purpose parallel row-wise normalization of
   an existing float32 buffer. Direct analog of
   `sklearn.preprocessing.normalize(X, norm='l2', axis=1)` (NumPy itself has
   no single built-in for this — `np.linalg.norm` + manual divide is the
   idiom being replaced). Useful for normalizing *any* float32 array a
   caller already has, not just freshly-generated ones.
3. **`generate_uniform_vectors`** — fused generate+normalize in a single Rust
   call, purpose-built for the VDB case and any other "generate a batch of
   unit vectors" workload. Internally composes (1) and (2) without the
   intermediate Python round-trip or extra full-buffer passes that calling
   them separately from Python would incur.

(1) and (2) are the "base functions" — general-purpose, reusable outside
VDB. (3) is the fused fast-path, expected to be the function VDB-style
callers actually use in production.

> Review note (2026-07-10): This split is reasonable, and it stays closer to
> dgen-rs's existing style if these remain low-level additive primitives
> returning `BytesView` / operating on buffer-protocol objects. I would avoid
> treating v1 as the start of a broader NumPy-compatibility layer; the smaller,
> more consistent target is better here.
>
> **Resolved**: v1 stays a small, focused, buffer-protocol-first addition —
> not the start of a general NumPy-compatibility layer. §7 (Future work)
> covers where a broader surface could grow later, deliberately kept out of
> scope here.

## 3. Proposed public API

All three follow the existing two-layer pattern: a `#[pyfunction]` in
`python_api.rs` returning a zero-copy `PyBytesView` (no new `numpy` Rust
crate dependency — consistent with how `generate_buffer`/`get_chunk` already
work), plus Python-side type stubs in `__init__.pyi` documenting the
`np.frombuffer(...).reshape(...)` idiom, matching how `BytesView` is already
documented and used today.

### 3.1 `generate_uniform` (base function)

```python
def generate_uniform(
    count: int,
    low: float = 0.0,
    high: float = 1.0,
    max_threads: Optional[int] = None,
    numa_mode: str = "auto",
    seed: Optional[int] = None,
) -> BytesView:
    """Generate `count` uniformly-distributed float32 values in [low, high).

    Analogous to numpy.random.uniform(low, high, size), but parallel
    (rayon, scales with max_threads) and returns a zero-copy BytesView —
    reinterpret via np.frombuffer(view, dtype=np.float32) for a flat
    array, or .reshape(...) for a specific shape.

    Uses the IEEE-754 bit-masking technique (mask to the 23 mantissa bits,
    force the exponent into [1.0, 2.0), subtract 1.0, then scale/shift by
    (high - low) and low) applied to dgen-py's existing Xoshiro256++ random
    byte stream — uses 100% of the generated random bits, no entropy loss,
    no rejection sampling.
    """
```

- `count` is an element count (not bytes) — matches `numpy.random.uniform`'s
  `size` parameter semantics, unlike `generate_buffer`'s byte-count `size`.
  Deliberately named `count`, not `size`, so it's never confused with
  `generate_buffer`'s byte-count `size` elsewhere in the same module (see
  resolution below).
- `low`/`high` default to `(0.0, 1.0)`, matching `numpy.random.random()`'s
  implicit range and `numpy.random.uniform()`'s defaults.
- No `dedup_ratio`/`compress_ratio` — those parameters exist for storage
  I/O-realism testing (dgen-py's original purpose) and have no meaning for a
  numeric-distribution function; omitting them keeps the signature honest
  about what it does.

> Review note (2026-07-10): I would not reuse the parameter name `size` if it
> means float32 element-count here but byte-count everywhere else in dgen-rs.
> If you want NumPy-like element semantics, rename it to `count` / `elements`
> instead of overloading `size` with two units across the API.
>
> **Resolved**: renamed to `count` throughout this design (function signature
> above already reflects it). `size` stays reserved, module-wide, for
> byte-counts only.

### 3.2 `normalize_rows` (base function)

```python
def normalize_rows(
    buffer: object,      # writable buffer: bytearray, memoryview, numpy array
    dim: int,
    max_threads: Optional[int] = None,
) -> None:
    """L2-normalize each row of a (rows, dim) float32 buffer, in place.

    buffer's length must be a multiple of dim * 4 bytes (float32). Rows are
    processed independently and in parallel (rayon par_chunks_mut over
    dim*4-byte row chunks) — same engine as the rest of dgen-py.

    Divides each row by its L2 (Euclidean) norm, i.e.
    row /= sqrt(sum(row**2)). A zero row is left unchanged (norm=0 would
    divide by zero) rather than raising, matching the common convention in
    similar libraries (e.g. scikit-learn).

    L2-only in v1 — no `norm` mode parameter (see §5, §7: L1/max-norm
    variants are additive future work, not pre-declared here).

    Operates IN PLACE for zero-copy efficiency — same philosophy as
    generate_into_buffer(). Accepts anything implementing the Python buffer
    protocol with write access, subject to the validation contract below.

    Raises ValueError if: buffer is read-only; buffer is not C-contiguous;
    buffer is a typed (e.g. NumPy) buffer whose format is not float32
    ('f'); dim == 0; or buffer's byte length is not a multiple of dim * 4.
    """
```

- In-place, not allocating — mirrors `generate_into_buffer`'s existing
  "operate into a provided buffer" philosophy, and is the cheapest possible
  semantics (no extra allocation, matches how `vectors /= norms` already
  works in the NumPy code being replaced).
- **L2-only, no `norm` parameter** — v1 does exactly one thing and says so;
  see resolution below.
- Takes any buffer-protocol object, not just dgen-py's own `BytesView` — so
  it's genuinely usable standalone on an arbitrary NumPy array a caller
  already has, not just on dgen-py's own output — subject to the tightened
  validation contract below.

**Input validation contract** (applies to `normalize_rows` and the
`normalize=True` path of `generate_uniform_vectors`, §3.3):
- `buffer` must be **writable** — reject read-only buffers (e.g. `bytes`,
  a read-only `memoryview`) with a clear `PyValueError`, not a silent no-op
  or a segfault.
- `buffer` must be **C-contiguous** — reject strided/non-contiguous views
  (e.g. a transposed NumPy array) rather than silently reinterpreting
  strided memory as flat bytes.
- `buffer` must be **logically float32**: if the buffer exposes a typed
  format via the buffer protocol (as NumPy arrays do — `format == 'f'` for
  `float32`, `'d'` for `float64`, etc.), it must be exactly `'f'`; a
  `float64` array must be rejected, not silently byte-reinterpreted. An
  untyped byte buffer (`bytearray`/`memoryview` of raw bytes, e.g. dgen-py's
  own `BytesView`/`generate_buffer()` output) is accepted as the expected
  "raw bytes to be reinterpreted as float32" case.
- `dim > 0`, and `buffer`'s byte length must be an exact multiple of
  `dim * 4` (a whole number of rows) — reject otherwise.
- All size arithmetic (`rows * dim`, `rows * dim * 4`) uses checked
  multiplication — overflow is a `PyValueError`, not a silent wraparound.

> Review note (2026-07-10): For a smaller and more honest v1, I would drop the
> `norm` parameter entirely and make `normalize_rows` explicitly L2-only. Adding
> a future optional keyword is additive; pre-declaring unsupported modes makes
> the surface look broader than the implementation.
>
> **Resolved**: `norm` parameter dropped. `normalize_rows` is explicitly
> L2-only in v1, as reflected in the signature and docstring above.
>
> Review note (2026-07-10): The buffer contract should also be tighter in the
> design text. Reusing `PyBuffer` is fine, but the wrapper should explicitly
> reject non-C-contiguous inputs and anything that is not logically a writable
> float32 matrix view. Otherwise a contiguous `float64` NumPy array or an odd
> byte buffer could be accepted and then interpreted as raw bytes, which would
> violate the intent of a numeric API.
>
> **Resolved**: validation contract spelled out explicitly above
> (writable + C-contiguous + logically-float32-or-untyped-bytes + shape/
> overflow checks), and carried into the §4.2 implementation-plan list and
> the §8 testing plan.

### 3.3 `generate_uniform_vectors` (fused function)

```python
def generate_uniform_vectors(
    rows: int,
    dim: int,
    low: float = 0.0,
    high: float = 1.0,
    normalize: bool = True,
    max_threads: Optional[int] = None,
    numa_mode: str = "auto",
    seed: Optional[int] = None,
) -> BytesView:
    """Generate `rows` vectors of `dim` uniformly-distributed float32
    elements in [low, high), optionally L2-normalized, in ONE Rust call.

    Equivalent to generate_uniform(rows*dim, low, high).reshape(rows, dim)
    followed by normalize_rows(..., dim) if normalize=True — but fused: no
    Python round-trip between generation and normalization, and the
    bit-mask + normalize passes share cache-warm access to the same buffer
    instead of two independent full-array sweeps.

    normalize=False skips normalization entirely (pure batched
    generate_uniform reshaped to (rows, dim) — useful when a caller wants
    unnormalized vectors, e.g. testing without the L2-unit-vector
    assumption). L2 is the only normalization this function performs — a
    boolean, not a mode string, since normalize_rows itself is L2-only in
    v1 (§3.2).

    Returns a zero-copy BytesView (not a NumPy array) — same
    np.frombuffer(view, dtype=np.float32).reshape(rows, dim) usage pattern
    as every other dgen-py function.

    This is the function VDB-style callers should use in production —
    see mlp-storage/vdb_benchmark/vdbbench/load_vdb.py::generate_vectors()
    for the motivating call site.
    """
```

- This is a strict superset of (1)+(2) composed — every parameter from both
  appears here, plus `rows`/`dim` replacing `count` (since row structure is
  required for normalization).
- `normalize=False` bypass keeps this useful even for VDB configs that
  don't want normalized vectors (e.g. `distribution` values other than what
  L2-normalization assumes — though today all three of `load_vdb.py`'s
  distributions get L2-normalized regardless of `distribution` choice, so
  in practice VDB always wants `normalize=True`, the default).
- **Returns `BytesView`, not a NumPy array** — see resolution below.

> Review note (2026-07-10): Keep this returning `BytesView` only in v1. The
> existing dgen-rs Python API is explicitly buffer-oriented, and keeping that
> pattern here avoids turning one new feature into a bigger API-shape change.
>
> **Resolved**: returns `BytesView`. `np.frombuffer(...).reshape(...)` is the
> documented usage pattern, stated directly in the docstring above. No
> Python-side NumPy-array convenience wrapper in v1 (see §6, open question 3
> — now resolved the same way).

## 4. Implementation plan

All three land in `generator.rs` (core logic) + `python_api.rs` (PyO3
wrapper), mirroring `generate_buffer`'s existing structure
(`python_api.rs:191-278`) as closely as possible, so the new code reads as
"the same pattern, new payload" rather than a new subsystem.

### 4.1 `generator.rs`: new core functions

```rust
/// Fill `buf` (must be a multiple of 4 bytes) with uniformly-distributed
/// float32 values in [low, high), using the existing block-parallel
/// Xoshiro256++ engine for the underlying random bits.
pub fn fill_uniform_f32(buf: &mut [u8], low: f32, high: f32, config: &GeneratorConfig) {
    // 1. Reuse the existing raw-byte block generator to fill `buf` (same
    //    par_chunks_mut(block_size) engine as generate_data()).
    // 2. A second par_chunks_mut(4) pass over the SAME buffer: reinterpret
    //    each 4-byte group as u32, mask to 23 mantissa bits, OR in the
    //    exponent for [1.0, 2.0), reinterpret as f32, subtract 1.0 (now
    //    [0,1)), then scale by (high - low) and add low.
}

/// Normalize each `dim`-float32-wide row of `buf` in place (L2 norm).
pub fn normalize_rows_f32(buf: &mut [u8], dim: usize, max_threads: Option<usize>) {
    // par_chunks_mut(dim * 4) over `buf` — one Rayon task per row:
    //   reinterpret chunk as &mut [f32], compute sum of squares, divide
    //   (skip if sum == 0.0).
}

/// Fused generate + optional normalize — single buffer, single pair of
/// parallel passes (no separate Python-visible intermediate).
pub fn generate_uniform_vectors_data(
    rows: usize, dim: usize, low: f32, high: f32,
    normalize: bool, config: &GeneratorConfig,
) -> DataBuffer {
    // allocate DataBuffer (existing type), fill_uniform_f32 into it, then
    // normalize_rows_f32 if normalize. Reuses DataBuffer/PyBytesView
    // exactly as generate_data() does today.
}
```

Key point: **step 1 of `fill_uniform_f32`** (raw byte generation) is not new
code — it already exists (`generate_data`'s block engine). The genuinely new
code is the bit-mask pass and the row-normalize pass, both simple,
embarrassingly-parallel numeric kernels using the exact same
`par_chunks_mut` idiom already proven in `generator.rs:664-683`. Estimated
~40-60 lines of new core logic across both kernels.

### 4.2 `python_api.rs`: PyO3 wrappers

Three `#[pyfunction]`s mirroring `generate_buffer`'s existing shape
(ratio-validation-free, since these don't take dedup/compress ratios):

```rust
#[pyfunction]
#[pyo3(signature = (count, low=0.0, high=1.0, max_threads=None, numa_mode="auto", seed=None))]
fn generate_uniform(py: Python<'_>, count: usize, low: f32, high: f32, ...) -> PyResult<Py<PyBytesView>> { ... }

#[pyfunction]
#[pyo3(signature = (buffer, dim, max_threads=None))]
fn normalize_rows(py: Python<'_>, buffer: &Bound<'_, PyAny>, dim: usize, ...) -> PyResult<()> { ... }

#[pyfunction]
#[pyo3(signature = (rows, dim, low=0.0, high=1.0, normalize=true, max_threads=None, numa_mode="auto", seed=None))]
fn generate_uniform_vectors(py: Python<'_>, rows: usize, dim: usize, ...) -> PyResult<Py<PyBytesView>> { ... }
```

- All three call `py.detach(|| ...)` around the actual work, same as
  `generate_buffer` (`python_api.rs:271`) — releases the GIL, gives true
  multi-core parallelism.
- `normalize_rows` takes the buffer via `PyBuffer` (same mechanism
  `generate_into_buffer` already uses to accept "bytearray, memoryview,
  numpy array, etc." per its own docstring) and requires write access —
  reject read-only buffers with a clear `PyValueError`.
- Register all three in `lib.rs`'s `#[pymodule]` block alongside the
  existing functions, and re-export from `python/dgen_py/__init__.py` /
  `__init__.pyi`, extending `__all__`.

**Locked-in input validation rules** (implemented as early returns /
`PyValueError`s in the PyO3 wrapper, before any generation/normalization
work starts):
- `low < high` (`generate_uniform`, `generate_uniform_vectors`).
- `dim > 0` (`normalize_rows`, `generate_uniform_vectors`).
- `rows * dim` and `rows * dim * 4` computed via checked multiplication —
  reject overflow rather than wrapping.
- `buffer.len() % (dim * 4) == 0` (`normalize_rows`) — whole number of rows.
- `buffer` writable, C-contiguous, and logically float32-or-untyped-bytes,
  per the full validation contract in §3.2.

> Review note (2026-07-10): This section should explicitly lock a few input
> validation rules into the design: `low < high`; `dim > 0`; `rows * dim`
> overflow checking; and `buffer.len() % (dim * 4) == 0`. These are small
> enough to define up front and will keep the implementation/review tighter.
>
> **Resolved**: rules locked in as listed immediately above, applied
> consistently across all three functions where relevant.

### 4.3 No new dependencies

No `numpy` Rust crate, no new Cargo dependency. Reuses `rayon` (already a
dependency), the existing `PyBytesView`/`PyBuffer` zero-copy plumbing, and
the existing `GeneratorConfig`/NUMA/thread-pool machinery unchanged.

## 5. Non-goals for v1

- **No other distributions** (normal/Gaussian, zipfian, etc.) in this pass.
  `generate_vectors()`'s `normal` and `zipfian` branches stay on NumPy for
  now. See §7 for why these are natural, low-effort follow-ups but
  deliberately out of scope here (keeping this PR reviewable and focused on
  the proven bottleneck).
- **No L1/max norm** — `normalize_rows` and `generate_uniform_vectors` are
  L2-only in v1; there is no `norm` mode parameter at all (dropped per
  review, §3.2). L1/max-norm variants are additive future work (§7), not
  pre-declared in the v1 signature.
- **No `float64` support** — `float32` only, matching what
  `generate_vectors()` and essentially all vector-DB / ANN-index workloads
  actually use. `f64` could be added later as a parallel set of functions if
  a real use case appears.
- **No changes to any existing function's signature or behavior.** Purely
  additive.

## 6. Resolved design decisions

(Originally posed as open questions; resolved via review, 2026-07-10 —
resolutions applied conservatively, favoring consistency with dgen-rs's
existing conventions over maximizing NumPy-familiarity.)

1. **Naming/namespace**: flat namespace, **not** a `dgen_py.random`
   submodule. `generate_uniform`/`normalize_rows`/`generate_uniform_vectors`
   sit alongside `generate_buffer`/`generate_data`/etc. exactly as proposed
   in §3. A submodule remains a possible future restructuring (§7) if the
   numeric surface grows enough to justify it, but is not part of v1.
2. **`count` vs. `size`**: resolved by renaming — `generate_uniform` takes
   `count` (element count), never overloading `generate_buffer`'s `size`
   (byte count) with a second unit. Applied throughout §3.1 and §4.2.
3. **Return type**: `BytesView` in all three functions, including
   `generate_uniform_vectors`. No Python-side NumPy-array convenience
   wrapper in v1 — `np.frombuffer(...).reshape(...)` stays the one
   documented usage pattern, consistent with every existing dgen-py
   function.
4. **Seed/determinism**: `seed=None` means non-deterministic (time+urandom
   entropy), matching `Generator.set_seed(None)`'s existing behavior,
   consistently across all three new functions.

## 7. Future work (not in this design's scope)

- `generate_normal` (Gaussian) via Box-Muller transform on two independent
  `generate_uniform` streams — same parallel infrastructure, well-understood
  algorithm, natural follow-up once the uniform/normalize primitives land
  and are validated.
- `normalize_rows` L1/max norm variants.
- A `dgen_py.random` submodule, only if the numeric surface grows enough
  later to justify revisiting the flat-namespace decision in §6 — not
  planned, just not permanently foreclosed.

## 8. Testing plan (per repo policy: RED-then-GREEN, verified not assumed)

Mirrors the existing `tests/test_generation.rs` / `tests/test_dgen_py_v024.py`
structure:

- **Correctness**: for `generate_uniform` — all values finite, within
  `[low, high)`, chi-squared / basic uniformity sanity check across bins.
  For `normalize_rows` — all output rows have L2 norm ≈ 1.0 (within float32
  tolerance) except genuine zero rows (left unchanged, not NaN). For
  `generate_uniform_vectors` — both of the above simultaneously, plus a
  byte-for-byte equivalence check against calling `generate_uniform` +
  `normalize_rows` separately with the same seed (proves the fused path is
  not just fast but *equivalent*).
- **Thread-count scaling**: a benchmark (new file under `benches/`,
  following `benches/streaming_throughput.rs`'s existing convention)
  sweeping `max_threads` at the VDB production size (1M × 1536). This is
  **benchmark/reporting coverage, not a CI-blocking correctness
  assertion** — it records throughput-vs-threads (the exact regression
  surface for the problem being fixed) without asserting a hard pass/fail
  threshold in CI, since performance thresholds are noisy across machines.
  Kept clearly separate from the functional RED/GREEN tests above, which
  assert correctness (finite, in-range, normalized) and are the only tests
  gating CI.
- **Zero-copy verification**: assert `normalize_rows` does not allocate a
  new buffer (existing buffer's `id()`/pointer unchanged before/after, on
  the Python side).
- Every new test confirmed RED against a stub/unimplemented version before
  the real implementation lands, per this project family's standing
  RED-then-GREEN policy.

> Review note (2026-07-10): I would keep throughput scaling as a benchmark and
> report, not as a pass/fail correctness assertion in CI. Performance-threshold
> tests tend to be noisy across machines; the design should distinguish clearly
> between functional RED/GREEN tests and performance benchmarking.
>
> **Resolved**: thread-scaling is benchmark/reporting only, explicitly not a
> CI-gating assertion, as reflected immediately above.
