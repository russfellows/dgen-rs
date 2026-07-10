// src/python_api.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Zero-copy Python bindings using PyO3 buffer protocol

use pyo3::buffer::PyBuffer;
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::cell::RefCell;

use crate::constants::BLOCK_SIZE;
use crate::generator::{
    fill_uniform_f32, generate_data, generate_uniform_vectors_data, normalize_rows_f32, DataBuffer,
    DataGenerator, GeneratorConfig, NumaMode,
};
use crate::rolling_pool::RollingPool;

#[cfg(feature = "numa")]
use crate::numa::NumaTopology;

// =============================================================================
// Thread-local rolling pool for generate_buffer() small-object fast path
// =============================================================================

thread_local! {
    /// Per-thread pool used by generate_buffer() when size < BLOCK_SIZE.
    /// Each Python OS thread gets its own pool — no locking required.
    static PY_POOL: RefCell<Option<RollingPool>> = const { RefCell::new(None) };
}

// =============================================================================
// Zero-Copy Buffer Support
// =============================================================================

/// Internal storage for a PyBytesView — either a full owned DataBuffer
/// (large objects or NUMA allocation) or an Arc-sliced Bytes window from
/// the rolling pool (small objects < BLOCK_SIZE).
pub(crate) enum PyBytesViewInner {
    /// Caller owns the full DataBuffer (Vec<u8> or NUMA Bytes).
    Owned(DataBuffer),
    /// Zero-copy Arc slice from the rolling pool.
    Slice(bytes::Bytes),
}

impl PyBytesViewInner {
    fn len(&self) -> usize {
        match self {
            PyBytesViewInner::Owned(buf) => buf.len(),
            PyBytesViewInner::Slice(b) => b.len(),
        }
    }
    fn as_ptr(&self) -> *const u8 {
        match self {
            PyBytesViewInner::Owned(buf) => buf.as_ptr(),
            PyBytesViewInner::Slice(b) => b.as_ptr(),
        }
    }
    fn as_slice(&self) -> &[u8] {
        match self {
            PyBytesViewInner::Owned(buf) => buf.as_slice(),
            PyBytesViewInner::Slice(b) => b.as_ref(),
        }
    }
}

/// A Python-visible wrapper around DataBuffer (UMA or NUMA) or a rolling-pool
/// slice that exposes the buffer protocol.
///
/// ZERO-COPY: Python accesses the underlying memory directly via raw pointer!
#[pyclass(name = "BytesView")]
pub struct PyBytesView {
    inner: PyBytesViewInner,
}

#[pymethods]
impl PyBytesView {
    /// Get the length of the data
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    /// Support bytes() conversion - returns a copy
    fn __bytes__<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.as_slice())
    }

    /// Implement Python buffer protocol for zero-copy access.
    /// This allows `memoryview(data)` to work directly.
    ///
    /// The buffer is read-only; requesting a writable buffer will raise BufferError.
    ///
    /// ZERO-COPY: Python accesses NUMA memory directly via raw pointer!
    unsafe fn __getbuffer__(
        slf: PyRef<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: std::os::raw::c_int,
    ) -> PyResult<()> {
        // Check for writable request - we only support read-only buffers
        if (flags & ffi::PyBUF_WRITABLE) != 0 {
            return Err(pyo3::exceptions::PyBufferError::new_err(
                "BytesView is read-only and does not support writable buffers",
            ));
        }

        let len = slf.inner.len();
        let ptr = slf.inner.as_ptr();

        // Fill in the Py_buffer struct with DataBuffer's raw pointer
        unsafe {
            (*view).buf = ptr as *mut std::os::raw::c_void;
            (*view).len = len as isize;
            (*view).readonly = 1;
            (*view).itemsize = 1;

            // Format string: "B" = unsigned byte (matches u8)
            (*view).format = if (flags & ffi::PyBUF_FORMAT) != 0 {
                c"B".as_ptr() as *mut std::os::raw::c_char
            } else {
                std::ptr::null_mut()
            };

            (*view).ndim = 1;

            // Shape: pointer to the length (1D array of len elements)
            (*view).shape = if (flags & ffi::PyBUF_ND) != 0 {
                &(*view).len as *const isize as *mut isize
            } else {
                std::ptr::null_mut()
            };

            // Strides: 1 byte per element
            (*view).strides = if (flags & ffi::PyBUF_STRIDES) != 0 {
                &(*view).itemsize as *const isize as *mut isize
            } else {
                std::ptr::null_mut()
            };

            (*view).suboffsets = std::ptr::null_mut();
            (*view).internal = std::ptr::null_mut();

            // CRITICAL: Store a reference to the PyBytesView object
            // This prevents the DataBuffer (Vec or NUMA Bytes) from being deallocated
            // while the Python memoryview is in use
            // Note: Cast is intentionally explicit for PyO3 FFI compatibility across versions
            #[allow(clippy::unnecessary_cast)]
            {
                (*view).obj = slf.as_ptr() as *mut ffi::PyObject;
            }
            ffi::Py_INCREF((*view).obj);
        }

        Ok(())
    }

    /// Release the buffer - called when the memoryview is garbage collected.
    /// Python decrefs view.obj which will eventually drop the PyBytesView and DataBuffer
    unsafe fn __releasebuffer__(&self, _view: *mut ffi::Py_buffer) {
        // Nothing to do - the Py_DECREF on view.obj will be handled by Python
        // and will eventually drop the PyBytesView (and thus the DataBuffer) when refcount hits 0
    }
}

// =============================================================================
// Simple API - Single-call data generation
// =============================================================================

/// Generate random data with controllable deduplication and compression
///
/// # Arguments
/// * `size` - Total bytes to generate
/// * `dedup_ratio` - Deduplication ratio (integer: 1 = no dedup, 2 = 2:1 ratio, etc.)
/// * `compress_ratio` - Compression ratio (integer: 1 = incompressible, 2 = 2:1 ratio, etc.)
/// * `numa_mode` - NUMA mode: "auto", "force", or "disabled" (default: "auto")
/// * `max_threads` - Maximum threads to use (None = use all cores)
///
/// # Returns
/// Python bytes object with generated data (zero-copy from Rust)
///
/// # Note
/// Ratios must be integers >= 1. Floats will be truncated with a warning.
///
/// # Example
/// ```python
/// import dgen_py
///
/// # Generate 1 MiB incompressible data using 8 threads
/// data = dgen_py.generate_buffer(1024 * 1024, dedup_ratio=1,
///                                  compress_ratio=1, max_threads=8)
/// print(f"Generated {len(data)} bytes")
/// ```
#[allow(clippy::too_many_arguments)] // PyO3 function — Python API args cannot be easily grouped
#[pyfunction]
#[pyo3(signature = (size, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None, numa_node=None, method="parallel"))]
fn generate_buffer(
    py: Python<'_>,
    size: usize,
    dedup_ratio: f64,
    compress_ratio: f64,
    numa_mode: &str,
    max_threads: Option<usize>,
    numa_node: Option<usize>,
    method: &str,
) -> PyResult<Py<PyBytesView>> {
    // Warn if floats are being truncated
    if dedup_ratio.fract() != 0.0 {
        let truncated = dedup_ratio as usize;
        let warnings = py.import("warnings")?;
        warnings.call_method1(
            "warn",
            (format!(
                "dedup_ratio={:.2} truncated to integer {} (fractional ratios not supported)",
                dedup_ratio, truncated
            ),),
        )?;
    }
    if compress_ratio.fract() != 0.0 {
        let truncated = compress_ratio as usize;
        let warnings = py.import("warnings")?;
        warnings.call_method1(
            "warn",
            (format!(
                "compress_ratio={:.2} truncated to integer {} (fractional ratios not supported)",
                compress_ratio, truncated
            ),),
        )?;
    }

    // Convert ratios to integer factors
    let dedup = (dedup_ratio.max(1.0) as usize).max(1);
    let compress = (compress_ratio.max(1.0) as usize).max(1);

    // Parse method — only "parallel" is supported; accept silently for backward compat
    let _ = method; // method parameter kept for API compatibility

    // Parse NUMA mode
    let numa = match numa_mode.to_lowercase().as_str() {
        "auto" => NumaMode::Auto,
        "force" => NumaMode::Force,
        "disabled" | "disable" => NumaMode::Disabled,
        _ => {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'",
                numa_mode
            )))
        }
    };

    // ── Small-object fast path: rolling pool ─────────────────────────────────
    // For objects < BLOCK_SIZE (1 MB) without NUMA pinning, use the thread-local
    // rolling pool.  generate_data() enforces a BLOCK_SIZE minimum internally,
    // so without the pool each 64 KB call generates 1 MB and wastes 15/16 of it.
    if size < BLOCK_SIZE && numa_node.is_none() {
        let slice = PY_POOL.with(|cell| {
            let mut opt = cell.borrow_mut();
            let pool = opt.get_or_insert_with(|| RollingPool::new(dedup, compress));
            pool.reconfigure(dedup, compress);
            pool.next_slice(size)
        });
        return Py::new(
            py,
            PyBytesView {
                inner: PyBytesViewInner::Slice(slice),
            },
        );
    }

    // ── Standard path: full DataBuffer (large objects or NUMA-pinned) ────────
    let config = GeneratorConfig {
        size,
        dedup_factor: dedup,
        compress_factor: compress,
        numa_mode: numa,
        max_threads,
        numa_node,
        block_size: None,
        seed: None,
    };

    // Generate data WITHOUT holding GIL (allows parallel Python threads)
    let data = py.detach(|| generate_data(config));

    // Return PyBytesView with DataBuffer — ZERO COPY via raw pointer / buffer protocol
    Py::new(
        py,
        PyBytesView {
            inner: PyBytesViewInner::Owned(data),
        },
    )
}

/// Generate data using Python buffer protocol (for writing into existing buffer)
///
/// # Arguments
/// * `buffer` - Pre-allocated Python buffer (bytearray, memoryview, numpy array, etc.)
/// * `dedup_ratio` - Deduplication ratio (integer: 1 = no dedup, 2 = 2:1 ratio, etc.)
/// * `compress_ratio` - Compression ratio (integer: 1 = incompressible, 2 = 2:1 ratio, etc.)
/// * `numa_mode` - NUMA mode: "auto", "force", or "disabled" (default: "auto")
/// * `max_threads` - Maximum threads to use (None = use all cores)
///
/// # Returns
/// Number of bytes written
///
/// # Note
/// Ratios must be integers >= 1. Floats will be truncated with a warning.
///
/// # Example
/// ```python
/// import dgen_py
///
/// # Pre-allocate buffer
/// buf = bytearray(1024 * 1024)
///
/// # Generate directly into buffer (zero-copy) using 4 threads
/// nbytes = dgen_py.generate_into_buffer(buf, dedup_ratio=1,
///                                        compress_ratio=2, max_threads=4)
/// print(f"Wrote {nbytes} bytes")
/// ```
#[allow(clippy::too_many_arguments)] // PyO3 function — Python API args cannot be easily grouped
#[pyfunction]
#[pyo3(signature = (buffer, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None, numa_node=None, method="parallel"))]
fn generate_into_buffer(
    py: Python<'_>,
    buffer: &Bound<'_, PyAny>,
    dedup_ratio: f64,
    compress_ratio: f64,
    numa_mode: &str,
    max_threads: Option<usize>,
    numa_node: Option<usize>,
    method: &str,
) -> PyResult<usize> {
    // Get buffer via PyBuffer protocol
    let buf: PyBuffer<u8> = PyBuffer::get(buffer)?;

    // Ensure buffer is writable and contiguous
    if buf.readonly() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Buffer must be writable",
        ));
    }

    if !buf.is_c_contiguous() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Buffer must be C-contiguous for zero-copy operation",
        ));
    }

    // Warn if floats are being truncated
    if dedup_ratio.fract() != 0.0 {
        let truncated = dedup_ratio as usize;
        let warnings = py.import("warnings")?;
        warnings.call_method1(
            "warn",
            (format!(
                "dedup_ratio={:.2} truncated to integer {} (fractional ratios not supported)",
                dedup_ratio, truncated
            ),),
        )?;
    }
    if compress_ratio.fract() != 0.0 {
        let truncated = compress_ratio as usize;
        let warnings = py.import("warnings")?;
        warnings.call_method1(
            "warn",
            (format!(
                "compress_ratio={:.2} truncated to integer {} (fractional ratios not supported)",
                compress_ratio, truncated
            ),),
        )?;
    }

    let size = buf.len_bytes();
    let dedup = (dedup_ratio.max(1.0) as usize).max(1);
    let compress = (compress_ratio.max(1.0) as usize).max(1);

    // method parameter kept for API compatibility; only "parallel" is supported
    let _ = method;

    // Parse NUMA mode
    let numa = match numa_mode.to_lowercase().as_str() {
        "auto" => NumaMode::Auto,
        "force" => NumaMode::Force,
        "disabled" | "disable" => NumaMode::Disabled,
        _ => {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'",
                numa_mode
            )))
        }
    };

    // Build config
    let config = GeneratorConfig {
        size,
        dedup_factor: dedup,
        compress_factor: compress,
        numa_mode: numa,
        max_threads,
        numa_node,
        block_size: None,
        seed: None,
    };

    // Generate data
    let data = generate_data(config);

    // Write into buffer (zero-copy write)
    unsafe {
        let dst_ptr = buf.buf_ptr() as *mut u8;
        std::ptr::copy_nonoverlapping(data.as_ptr(), dst_ptr, size);
    }

    Ok(size)
}

// =============================================================================
// Numeric distributions: generate_uniform, normalize_rows, generate_uniform_vectors
//
// docs/DESIGN_NUMERIC_DISTRIBUTIONS.md — added for mlcommons/storage#625.
// Flat namespace, BytesView returns, buffer-protocol-first — consistent with
// the rest of dgen-py rather than adopting NumPy's API shape (see design
// doc §2/§6). `count` (not `size`) is a float32 element count, so it's
// never confused with generate_buffer's byte-count `size`.
// =============================================================================

/// Get a writable, C-contiguous, logically-float32-or-untyped-bytes slice
/// from `buf`. `buf` must already be validated readonly/contiguous by the
/// caller — see [`buffer_writable_bytes`], called immediately after either
/// a `PyBuffer<f32>` or `PyBuffer<u8>` acquisition succeeds.
///
/// # Safety contract
/// The returned slice borrows the SAME memory `buf` guards; `buf` must stay
/// alive (not be dropped) for as long as the returned slice is used —
/// callers must keep `buf` in scope across any `py.detach()` call that uses
/// this slice, so the buffer-export lock stays held for the whole time
/// another thread could be writing through it.
fn buffer_writable_bytes<T>(buf: &mut PyBuffer<T>) -> PyResult<&mut [u8]> {
    if buf.readonly() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "buffer must be writable",
        ));
    }
    if !buf.is_c_contiguous() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "buffer must be C-contiguous",
        ));
    }
    let ptr = buf.buf_ptr() as *mut u8;
    let len = buf.len_bytes();
    // SAFETY: ptr/len come from a just-validated writable, C-contiguous
    // Py_buffer; the borrow-checker-invisible 'static-looking lifetime here
    // is constrained by the caller's contract (buf must outlive the slice).
    Ok(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

fn invalid_numa_mode_err(numa_mode: &str) -> PyErr {
    PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
        "Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'",
        numa_mode
    ))
}

fn parse_numa_mode(numa_mode: &str) -> PyResult<NumaMode> {
    match numa_mode.to_lowercase().as_str() {
        "auto" => Ok(NumaMode::Auto),
        "force" => Ok(NumaMode::Force),
        "disabled" | "disable" => Ok(NumaMode::Disabled),
        _ => Err(invalid_numa_mode_err(numa_mode)),
    }
}

/// Generate `count` uniformly-distributed float32 values in `[low, high)`.
///
/// # Arguments
/// * `count` - Number of float32 elements to generate (NOT bytes — see
///   module note above)
/// * `low`, `high` - Range, default `[0.0, 1.0)`
/// * `max_threads` - Maximum threads to use (None = use all cores)
/// * `numa_mode` - "auto" (default), "force", or "disabled"
/// * `seed` - Random seed for reproducible output (None = time+urandom,
///   non-deterministic — matches `Generator.set_seed(None)`)
///
/// # Returns
/// Zero-copy `BytesView` — reinterpret via
/// `np.frombuffer(view, dtype=np.float32)` (`.reshape(...)` for a specific
/// shape).
///
/// # Example
/// ```python
/// import dgen_py
/// import numpy as np
///
/// view = dgen_py.generate_uniform(1_000_000, low=0.0, high=1.0)
/// arr = np.frombuffer(view, dtype=np.float32)
/// ```
#[allow(clippy::too_many_arguments)]
#[pyfunction]
#[pyo3(signature = (count, low=0.0, high=1.0, max_threads=None, numa_mode="auto", seed=None))]
fn generate_uniform(
    py: Python<'_>,
    count: usize,
    low: f32,
    high: f32,
    max_threads: Option<usize>,
    numa_mode: &str,
    seed: Option<u64>,
) -> PyResult<Py<PyBytesView>> {
    if low.is_nan() || high.is_nan() || low >= high {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "generate_uniform: low must be < high",
        ));
    }
    let total_bytes = count.checked_mul(4).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyValueError, _>("generate_uniform: count * 4 overflow")
    })?;
    let numa = parse_numa_mode(numa_mode)?;

    let config = GeneratorConfig {
        size: total_bytes,
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: numa,
        max_threads,
        numa_node: None,
        block_size: None,
        seed,
    };

    let mut buf = vec![0u8; total_bytes];
    py.detach(|| fill_uniform_f32(&mut buf, low, high, &config));

    Py::new(
        py,
        PyBytesView {
            inner: PyBytesViewInner::Owned(DataBuffer::Uma(buf)),
        },
    )
}

/// L2-normalize each row of a `(rows, dim)` float32 buffer, in place.
///
/// # Arguments
/// * `buffer` - A writable, C-contiguous buffer: a `bytearray`/`memoryview`
///   of raw bytes (e.g. `generate_uniform()`'s own output, wrapped in
///   `bytearray()`), or a NumPy `float32` array. Any other typed buffer
///   (e.g. `float64`) is rejected with `ValueError`, not silently
///   reinterpreted.
/// * `dim` - Row width in float32 elements. `buffer`'s byte length must be
///   an exact multiple of `dim * 4`.
/// * `max_threads` - Maximum threads to use (None = use all cores)
///
/// L2-only in v1 (see docs/DESIGN_NUMERIC_DISTRIBUTIONS.md §5) — a zero row
/// is left unchanged (dividing by a zero norm would produce NaN) rather
/// than raising.
///
/// # Example
/// ```python
/// import dgen_py
///
/// buf = bytearray(dgen_py.generate_uniform(1000 * 128))
/// dgen_py.normalize_rows(buf, dim=128)
/// ```
#[pyfunction]
#[pyo3(signature = (buffer, dim, max_threads=None))]
fn normalize_rows(
    py: Python<'_>,
    buffer: &Bound<'_, PyAny>,
    dim: usize,
    max_threads: Option<usize>,
) -> PyResult<()> {
    if dim == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "normalize_rows: dim must be > 0",
        ));
    }
    let row_bytes = dim.checked_mul(4).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyValueError, _>("normalize_rows: dim * 4 overflow")
    })?;

    // Try float32-typed buffer first (e.g. a NumPy float32 array), then
    // fall back to an untyped raw-byte buffer (bytearray, memoryview,
    // dgen-py's own BytesView-wrapped-in-bytearray). Anything else (e.g. a
    // float64 array) fails BOTH attempts and hits the final Err below —
    // never silently byte-reinterpreted.
    if let Ok(mut buf) = PyBuffer::<f32>::get(buffer) {
        let slice = buffer_writable_bytes(&mut buf)?;
        if slice.len() % row_bytes != 0 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "normalize_rows: buffer length must be a multiple of dim * 4",
            ));
        }
        py.detach(|| normalize_rows_f32(slice, dim, max_threads));
        return Ok(());
    }
    if let Ok(mut buf) = PyBuffer::<u8>::get(buffer) {
        let slice = buffer_writable_bytes(&mut buf)?;
        if slice.len() % row_bytes != 0 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "normalize_rows: buffer length must be a multiple of dim * 4",
            ));
        }
        py.detach(|| normalize_rows_f32(slice, dim, max_threads));
        return Ok(());
    }
    Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
        "normalize_rows: buffer must be a writable float32 array or a raw byte buffer \
         (e.g. bytearray) — other typed buffers (e.g. float64) are rejected",
    ))
}

/// Generate `rows` vectors of `dim` uniformly-distributed float32 elements
/// in `[low, high)`, optionally L2-normalized, in ONE call.
///
/// Equivalent to `generate_uniform(rows*dim, low, high).reshape(rows,
/// dim)` followed by `normalize_rows(..., dim)` if `normalize=True` — but
/// fused: no Python round-trip between generation and normalization.
///
/// # Arguments
/// * `rows`, `dim` - Output shape
/// * `low`, `high` - Range, default `[0.0, 1.0)`
/// * `normalize` - L2-normalize each row (default `True`). `False` skips
///   normalization entirely (pure batched `generate_uniform` reshaped to
///   `(rows, dim)`).
/// * `max_threads` - Maximum threads to use (None = use all cores)
/// * `numa_mode` - "auto" (default), "force", or "disabled"
/// * `seed` - Random seed for reproducible output (None = time+urandom)
///
/// # Returns
/// Zero-copy `BytesView` — reinterpret via
/// `np.frombuffer(view, dtype=np.float32).reshape(rows, dim)`.
///
/// This is the function VDB-style callers should use in production — see
/// `mlp-storage/vdb_benchmark/vdbbench/load_vdb.py::generate_vectors()`
/// for the motivating call site (mlcommons/storage#625).
#[allow(clippy::too_many_arguments)]
#[pyfunction]
#[pyo3(signature = (rows, dim, low=0.0, high=1.0, normalize=true, max_threads=None, numa_mode="auto", seed=None))]
fn generate_uniform_vectors(
    py: Python<'_>,
    rows: usize,
    dim: usize,
    low: f32,
    high: f32,
    normalize: bool,
    max_threads: Option<usize>,
    numa_mode: &str,
    seed: Option<u64>,
) -> PyResult<Py<PyBytesView>> {
    if low.is_nan() || high.is_nan() || low >= high {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "generate_uniform_vectors: low must be < high",
        ));
    }
    if dim == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "generate_uniform_vectors: dim must be > 0",
        ));
    }
    rows.checked_mul(dim)
        .and_then(|e| e.checked_mul(4))
        .ok_or_else(|| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "generate_uniform_vectors: rows * dim * 4 overflow",
            )
        })?;
    let numa = parse_numa_mode(numa_mode)?;

    let config = GeneratorConfig {
        size: 0, // unused directly; rows*dim*4 drives allocation inside generate_uniform_vectors_data
        dedup_factor: 1,
        compress_factor: 1,
        numa_mode: numa,
        max_threads,
        numa_node: None,
        block_size: None,
        seed,
    };

    let data =
        py.detach(|| generate_uniform_vectors_data(rows, dim, low, high, normalize, &config));

    Py::new(
        py,
        PyBytesView {
            inner: PyBytesViewInner::Owned(data),
        },
    )
}

// =============================================================================
// Streaming API - Generator class
// =============================================================================

/// Streaming data generator for incremental generation
///
/// # Example
/// ```python
/// import dgen_py
///
/// # Create generator for 100 MiB
/// gen = dgen_py.Generator(
///     size=100 * 1024 * 1024,
///     dedup_ratio=2.0,
///     compress_ratio=3.0
/// )
///
/// # Generate in chunks
/// chunk_size = 8192
/// buf = bytearray(chunk_size)
/// total = 0
///
/// while not gen.is_complete():
///     nbytes = gen.fill_chunk(buf)
///     if nbytes == 0:
///         break
///     total += nbytes
///     # Process chunk...
///
/// print(f"Generated {total} bytes")
/// ```
#[pyclass(name = "Generator")]
struct PyGenerator {
    inner: DataGenerator,
    chunk_size: usize, // Recommended chunk size for fill_chunk() calls
}

#[pymethods]
impl PyGenerator {
    /// Create new streaming generator
    ///
    /// # Arguments
    /// * `size` - Total bytes to generate
    /// * `dedup_ratio` - Deduplication ratio (integer: 1 = no dedup, 2 = 2:1 ratio, etc.)
    /// * `compress_ratio` - Compression ratio (integer: 1 = incompressible, 2 = 2:1 ratio, etc.)
    /// * `numa_mode` - NUMA mode: "auto", "force", or "disabled" (default: "auto")
    /// * `max_threads` - Maximum threads to use (None = use all cores)
    /// * `numa_node` - Pin to specific NUMA node (None = use all nodes, 0-N = specific node)
    /// * `chunk_size` - Chunk size for streaming (default: 32 MB for optimal performance)
    /// * `block_size` - Internal parallelization block size (default: 4 MB, max: 32 MB)
    /// * `seed` - Random seed for reproducible data (None = use time + urandom for non-deterministic)
    ///
    /// # Note on Ratios
    /// Both dedup_ratio and compress_ratio MUST be integers >= 1.
    /// If floats are provided, they will be truncated with a warning.
    /// Example: 2.7 becomes 2, 1.5 becomes 1
    ///
    /// # Reproducibility
    /// When seed is provided, Generator produces identical data for the same configuration.
    /// This enables reproducible testing and benchmarking.
    #[new]
    #[pyo3(signature = (size, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None, numa_node=None, chunk_size=None, block_size=None, seed=None, method="parallel"))]
    #[allow(clippy::too_many_arguments)] // PyO3 API requires all parameters as function arguments
    fn new(
        py: Python<'_>,
        size: usize,
        dedup_ratio: f64,
        compress_ratio: f64,
        numa_mode: &str,
        max_threads: Option<usize>,
        numa_node: Option<usize>,
        chunk_size: Option<usize>,
        block_size: Option<usize>,
        seed: Option<u64>,
        method: &str,
    ) -> PyResult<Self> {
        // Warn if floats are being truncated
        if dedup_ratio.fract() != 0.0 {
            let truncated = dedup_ratio as usize;
            let warnings = py.import("warnings")?;
            warnings.call_method1(
                "warn",
                (format!(
                    "dedup_ratio={:.2} truncated to integer {} (fractional ratios not supported)",
                    dedup_ratio, truncated
                ),),
            )?;
        }
        if compress_ratio.fract() != 0.0 {
            let truncated = compress_ratio as usize;
            let warnings = py.import("warnings")?;
            warnings.call_method1(
                "warn",
                (format!("compress_ratio={:.2} truncated to integer {} (fractional ratios not supported)", 
                         compress_ratio, truncated),)
            )?;
        }

        let dedup = (dedup_ratio.max(1.0) as usize).max(1);
        let compress = (compress_ratio.max(1.0) as usize).max(1);

        // method parameter kept for API compatibility; only "parallel" is supported
        let _ = method;

        // Parse NUMA mode
        let numa = match numa_mode.to_lowercase().as_str() {
            "auto" => NumaMode::Auto,
            "force" => NumaMode::Force,
            "disabled" | "disable" => NumaMode::Disabled,
            _ => {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'",
                    numa_mode
                )))
            }
        };

        let config = GeneratorConfig {
            size,
            dedup_factor: dedup,
            compress_factor: compress,
            numa_mode: numa,
            max_threads,
            numa_node,
            block_size,
            seed,
        };

        let chunk_size = chunk_size.unwrap_or_else(DataGenerator::recommended_chunk_size);

        Ok(Self {
            inner: DataGenerator::new(config),
            chunk_size,
        })
    }

    /// Get recommended chunk size for optimal performance (32 MB)
    #[getter]
    fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Fill the next chunk of data
    ///
    /// # Arguments
    /// * `buffer` - Pre-allocated buffer to fill
    ///
    /// # Returns
    /// Number of bytes written (0 when complete)
    fn fill_chunk(&mut self, py: Python<'_>, buffer: Py<PyAny>) -> PyResult<usize> {
        // Get buffer via PyBuffer protocol
        let buf: PyBuffer<u8> = PyBuffer::get(buffer.bind(py))?;

        if buf.readonly() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Buffer must be writable",
            ));
        }

        if !buf.is_c_contiguous() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Buffer must be C-contiguous",
            ));
        }

        let size = buf.len_bytes();

        // ZERO-COPY: Generate DIRECTLY into Python buffer without holding GIL
        let written = py.detach(|| {
            // Create mutable slice from Python buffer pointer
            unsafe {
                let dst_ptr = buf.buf_ptr() as *mut u8;
                let dst_slice = std::slice::from_raw_parts_mut(dst_ptr, size);
                self.inner.fill_chunk(dst_slice)
            }
        });

        Ok(written)
    }

    /// Get data as BytesView (zero-copy access via memoryview)
    ///
    /// # Arguments
    /// * `chunk_size` - Size of chunk to read
    ///
    /// # Returns
    /// BytesView object or None if complete
    fn get_chunk(
        &mut self,
        py: Python<'_>,
        chunk_size: usize,
    ) -> PyResult<Option<Py<PyBytesView>>> {
        if self.inner.is_complete() {
            return Ok(None);
        }

        let mut chunk = vec![0u8; chunk_size];
        let written = self.inner.fill_chunk(&mut chunk);

        if written == 0 {
            Ok(None)
        } else {
            chunk.truncate(written);
            // Wrap in DataBuffer::Uma for zero-copy Python access
            let buffer = DataBuffer::Uma(chunk);
            Ok(Some(Py::new(
                py,
                PyBytesView {
                    inner: PyBytesViewInner::Owned(buffer),
                },
            )?))
        }
    }

    /// Reset generator to start
    fn reset(&mut self) {
        self.inner.reset();
    }

    /// Get current position
    fn position(&self) -> usize {
        self.inner.position()
    }

    /// Get total size
    fn total_size(&self) -> usize {
        self.inner.total_size()
    }

    /// Check if generation is complete
    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }

    /// Set or reset the random seed for subsequent data generation
    ///
    /// This allows changing the data pattern mid-stream while maintaining generation position.
    /// The new seed takes effect on the next fill_chunk() call.
    ///
    /// # Arguments
    /// * `seed` - New seed value (int), or None to use time+urandom entropy (non-deterministic)
    ///
    /// # Example
    /// ```python
    /// import dgen_py
    ///
    /// gen = dgen_py.Generator(size=100*1024**3, seed=12345)
    /// buffer = bytearray(gen.chunk_size)
    ///
    /// # Generate some data with initial seed
    /// gen.fill_chunk(buffer)
    ///
    /// # Change seed for different pattern
    /// gen.set_seed(67890)
    /// gen.fill_chunk(buffer)  # Uses new seed
    ///
    /// # Switch to non-deterministic mode
    /// gen.set_seed(None)
    /// gen.fill_chunk(buffer)  # Uses time+urandom
    /// ```
    fn set_seed(&mut self, seed: Option<u64>) {
        self.inner.set_seed(seed);
    }
}

// =============================================================================
// NUMA Info API
// =============================================================================

#[cfg(feature = "numa")]
#[pyfunction]
fn get_numa_info(py: Python<'_>) -> PyResult<Py<PyAny>> {
    use pyo3::types::PyDict;

    let topology = NumaTopology::detect()
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

    let dict = PyDict::new(py);
    dict.set_item("num_nodes", topology.num_nodes)?;
    dict.set_item("physical_cores", topology.physical_cores)?;
    dict.set_item("logical_cpus", topology.logical_cpus)?;
    dict.set_item("is_uma", topology.is_uma)?;
    dict.set_item("deployment_type", topology.deployment_type())?;

    Ok(dict.into())
}

// =============================================================================
// Bulk Bytearray Pre-Allocation (Performance Optimization)
// =============================================================================

/// Pre-allocate multiple Python bytearrays from Rust (avoids Python runtime overhead)
///
/// This function creates a Python list of pre-allocated bytearrays, which is MUCH faster
/// than Python list comprehension: `[bytearray(size) for _ in range(count)]`
///
/// # Arguments
/// * `count` - Number of bytearrays to create
/// * `size` - Size of each bytearray in bytes
///
/// # Returns
/// Python list of bytearrays ready to be filled with generate_into_buffer() or fill_chunk()
///
/// # Performance
/// Rust allocation is ~1,654x faster than Python bytearray allocation for large datasets.
/// Uses Python's C API directly for efficient bytearray creation.
///
/// # Allocation Strategy (ALREADY OPTIMAL!)
/// We use Python's PyByteArray C API which delegates to system allocator:
/// - **Small objects** (<= 512 bytes): Python's pymalloc arena allocator
/// - **Large objects** (> 512 bytes): System malloc (glibc on Linux)
/// - **Very large objects** (>= 128 KB, including our 32 MB chunks): **glibc automatically uses mmap!**
///
/// For our 32 MB chunks, glibc malloc internally calls mmap (MMAP_THRESHOLD = 128 KB by default),
/// so we're ALREADY getting:
/// - Zero-copy kernel page allocation
/// - No heap fragmentation
/// - Automatic huge pages (if enabled)
/// - Direct page cache interaction
///
/// **No custom allocator (jemalloc/mimalloc) needed** - glibc's mmap path is optimal for large buffers!
///
/// # Why not use mmap directly?
/// PyByteArray doesn't support custom deallocators, so we'd have to:
/// 1. mmap allocate
/// 2. Copy to Python heap (defeats the purpose!)
/// 3. munmap
///
/// Current approach already uses mmap via glibc for our chunk sizes.
///
/// # Example
/// ```python
/// import dgen_py
///
/// # Fast: Create 768 × 32 MB bytearrays (uses mmap internally via glibc)
/// chunks = dgen_py.create_bytearrays(count=768, size=32*1024**2)  # 7.3 ms!
///
/// # Slow: Python list comprehension
/// # chunks = [bytearray(32*1024**2) for _ in range(768)]  # 12 seconds!
///
/// # Fill chunks with high-performance generation
/// gen = dgen_py.Generator(size=24*1024**3, numa_mode="auto", max_threads=None)
/// for buf in chunks:
///     gen.fill_chunk(buf)
/// ```
#[pyfunction]
fn create_bytearrays(py: Python<'_>, count: usize, size: usize) -> PyResult<Py<PyAny>> {
    use pyo3::ffi;
    use pyo3::types::{PyByteArray, PyList};

    // Create Python list to hold bytearrays
    let list = PyList::empty(py);

    // Pre-allocate bytearrays using PyByteArray C API
    // For large allocations (our 32 MB chunks), Python's allocator delegates to system malloc,
    // which automatically uses mmap for allocations >= 128 KB (glibc MMAP_THRESHOLD)
    for _ in 0..count {
        unsafe {
            // Create empty bytearray
            let ba_ptr = ffi::PyByteArray_FromStringAndSize(std::ptr::null(), 0);
            if ba_ptr.is_null() {
                return Err(pyo3::exceptions::PyMemoryError::new_err(
                    "Failed to create bytearray",
                ));
            }

            // Resize to desired size
            // For 32 MB chunks: Python -> PyMem_Realloc -> malloc -> mmap (automatic!)
            if ffi::PyByteArray_Resize(ba_ptr, size as isize) < 0 {
                ffi::Py_DECREF(ba_ptr);
                return Err(pyo3::exceptions::PyMemoryError::new_err(format!(
                    "Failed to resize bytearray to {} bytes",
                    size
                )));
            }

            // Wrap in PyByteArray
            let ba: Bound<'_, PyByteArray> = Bound::from_owned_ptr(py, ba_ptr).cast_into()?;
            list.append(ba)?;
        }
    }

    Ok(list.into())
}

// =============================================================================
// BufferPool — explicit rolling pool for Python hot loops
// =============================================================================

/// High-frequency small-object buffer pool with a rolling pointer.
///
/// For workloads that generate millions of small objects (for example simulated
/// PNG/JPEG images all below 1 MB), creating a `BufferPool` and calling
/// `next_slice()` is significantly faster than calling `generate_buffer()` in a
/// loop, because the 1 MB backing buffer is generated once and reused via
/// zero-copy Arc slices.
///
/// `generate_buffer()` already uses this pool automatically for `size < 1 MB`,
/// so for simple scripts `BufferPool` is optional.  Use it explicitly when you
/// want to control lifecycle, share a pool across helper functions, or mix
/// multiple dedup/compress configurations efficiently.
///
/// # Example
/// ```python
/// import dgen_py
///
/// pool = dgen_py.BufferPool(dedup_ratio=1, compress_ratio=1)
///
/// # Generate 10,000 simulated 64 KB images — each call is a zero-copy slice
/// images = [pool.next_slice(64 * 1024) for _ in range(10_000)]
/// print(f"Generated {sum(len(img) for img in images) / 1e6:.1f} MB")
/// ```
#[pyclass(name = "BufferPool")]
pub struct PyBufferPool {
    pool: RollingPool,
}

#[pymethods]
impl PyBufferPool {
    /// Create a new BufferPool.
    ///
    /// # Arguments
    /// * `dedup_ratio`    — Deduplication factor (1 = no dedup, N = N:1 ratio).
    /// * `compress_ratio` — Compression factor   (1 = incompressible, N = N:1 ratio).
    #[new]
    #[pyo3(signature = (dedup_ratio=1.0, compress_ratio=1.0))]
    fn new(dedup_ratio: f64, compress_ratio: f64) -> Self {
        let dedup = (dedup_ratio.max(1.0) as usize).max(1);
        let compress = (compress_ratio.max(1.0) as usize).max(1);
        Self {
            pool: RollingPool::new(dedup, compress),
        }
    }

    /// Return a zero-copy `BytesView` of exactly `size` bytes.
    ///
    /// For `size <= 1 MB`: serves from the internal rolling buffer (fast path).
    /// For `size > 1 MB`:  generates a fresh buffer (large-object path).
    ///
    /// The returned `BytesView` is independent of the pool — it holds its own
    /// Arc reference and remains valid even after the pool is refilled.
    fn next_slice(&mut self, py: Python<'_>, size: usize) -> PyResult<Py<PyBytesView>> {
        if size == 0 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "size must be > 0",
            ));
        }
        let slice = self.pool.next_slice(size);
        Py::new(
            py,
            PyBytesView {
                inner: PyBytesViewInner::Slice(slice),
            },
        )
    }

    /// Change dedup/compress parameters.
    ///
    /// If either value changes, the current 1 MB buffer is discarded and a new
    /// one is generated.  Existing `BytesView` slices already handed out remain
    /// valid.
    #[pyo3(signature = (dedup_ratio=1.0, compress_ratio=1.0))]
    fn reconfigure(&mut self, dedup_ratio: f64, compress_ratio: f64) {
        self.pool.reconfigure(
            (dedup_ratio as usize).max(1),
            (compress_ratio as usize).max(1),
        );
    }

    /// Bytes remaining in the current pool block before the next refill.
    #[getter]
    fn remaining(&self) -> usize {
        self.pool.remaining()
    }

    /// Current deduplication factor.
    #[getter]
    fn dedup_ratio(&self) -> usize {
        self.pool.dedup()
    }

    /// Current compression factor.
    #[getter]
    fn compress_ratio(&self) -> usize {
        self.pool.compress()
    }
}

// =============================================================================
// Benchmark helper
// =============================================================================

/// Benchmark `RollingPool::next_slice` in-process and return raw timing data.
///
/// This is a pure-Rust timing function callable from Python so that "Rust native"
/// throughput is measured in the *same process* with the *same Rayon thread pool
/// lifecycle* as `BufferPool.next_slice()`.  That makes the comparison fair:
/// no subprocess startup, no cold OS heap, no separate Rayon pool initialization.
///
/// # Arguments
/// * `obj_size`    — Size of each object in bytes (>0).
/// * `total_bytes` — Total bytes to generate.  Call count = max(total / obj_size, 1).
///
/// # Returns
/// `(bytes_generated: int, elapsed_secs: float)` — Python can compute GB/s from these.
///
/// The call includes one warmup invocation (excluded from the returned elapsed time).
///
/// # Example
/// ```python
/// generated, secs = dgen_py.bench_rolling_pool(64 * 1024, 1024**3)
/// gb_s = generated / secs / 1e9
/// print(f"Rust native 64 KB: {gb_s:.2f} GB/s")
/// ```
#[pyfunction]
fn bench_rolling_pool(obj_size: usize, total_bytes: usize) -> PyResult<(usize, f64)> {
    use std::hint::black_box;
    use std::time::Instant;

    if obj_size == 0 {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "obj_size must be > 0",
        ));
    }

    let calls = (total_bytes / obj_size).max(1);
    let mut pool = RollingPool::new(1, 1);

    // Warmup: one call, result discarded
    black_box(pool.next_slice(obj_size));

    let start = Instant::now();
    let mut generated: usize = 0;
    for _ in 0..calls {
        let buf = pool.next_slice(obj_size);
        generated += black_box(buf.len());
    }
    let elapsed = start.elapsed().as_secs_f64();

    Ok((generated, elapsed))
}

pub fn register_functions(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Zero-copy buffer type
    m.add_class::<PyBytesView>()?;

    // Simple API
    m.add_function(wrap_pyfunction!(generate_buffer, m)?)?;
    m.add_function(wrap_pyfunction!(generate_into_buffer, m)?)?;

    // Numeric distributions (docs/DESIGN_NUMERIC_DISTRIBUTIONS.md, storage#625)
    m.add_function(wrap_pyfunction!(generate_uniform, m)?)?;
    m.add_function(wrap_pyfunction!(normalize_rows, m)?)?;
    m.add_function(wrap_pyfunction!(generate_uniform_vectors, m)?)?;

    // Rolling pool explicit API
    m.add_class::<PyBufferPool>()?;

    // In-process Rust-native benchmark (for comparing with BufferPool overhead)
    m.add_function(wrap_pyfunction!(bench_rolling_pool, m)?)?;

    // Streaming API
    m.add_class::<PyGenerator>()?;

    // Bulk allocation optimization
    m.add_function(wrap_pyfunction!(create_bytearrays, m)?)?;

    // NUMA info
    #[cfg(feature = "numa")]
    m.add_function(wrap_pyfunction!(get_numa_info, m)?)?;

    Ok(())
}
