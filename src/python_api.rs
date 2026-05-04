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
use crate::generator::{generate_data, DataBuffer, DataGenerator, GeneratorConfig, NumaMode};
use crate::rolling_pool::RollingPool;
use crate::xor_stream::UniqueXorStream;

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
#[pyfunction]
#[pyo3(signature = (size, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None, numa_node=None))]
fn generate_buffer(
    py: Python<'_>,
    size: usize,
    dedup_ratio: f64,
    compress_ratio: f64,
    numa_mode: &str,
    max_threads: Option<usize>,
    numa_node: Option<usize>,
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
    // Config is not built on this path to avoid an unused-variable warning.
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

    // ── Standard path: full DataBuffer (large objects or NUMA pinned) ────────
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
#[pyfunction]
#[pyo3(signature = (buffer, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None, numa_node=None))]
fn generate_into_buffer(
    py: Python<'_>,
    buffer: &Bound<'_, PyAny>,
    dedup_ratio: f64,
    compress_ratio: f64,
    numa_mode: &str,
    max_threads: Option<usize>,
    numa_node: Option<usize>,
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
        numa_node, // CRITICAL: Bind to specific NUMA node if specified
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
    #[pyo3(signature = (size, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None, numa_node=None, chunk_size=None, block_size=None, seed=None))]
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
// XorStream — fast, dedup-safe data generation without Rayon
// =============================================================================

/// Fast, dedup-safe data generator using XOR keystream.
///
/// A `XorStream` holds a 1 MiB random base buffer and an atomic counter.
/// Each `fill()` or `generate()` call produces a unique output block — no
/// two calls ever share a 512-byte fingerprint, guaranteed.
///
/// **Thread-safe**: every method takes `&self`, so the same instance can be
/// shared across Python threads without a mutex.
///
/// **When to prefer `XorStream` over `Generator`**:
/// - High concurrency (≥ 32 concurrent callers): no Rayon thread scheduling overhead
/// - Medium objects (1–32 MiB): single-core throughput ~15 GB/s
/// - Dedup-safe requirement: each call produces provably unique data
///
/// # Rust example
///
/// ```rust
/// use dgen_data::UniqueXorStream;
///
/// let stream = UniqueXorStream::new();
/// let mut buf = vec![0u8; 8 * 1024 * 1024];
/// stream.fill(&mut buf);   // object 0 — unique 8 MiB payload
/// stream.fill(&mut buf);   // object 1 — completely different bytes
/// ```
///
/// # Python example
///
/// ```python
/// import dgen_py
///
/// stream = dgen_py.XorStream()
///
/// # Fill a pre-allocated bytearray in-place (no allocation on hot path)
/// buf = bytearray(8 * 1024 * 1024)
/// stream.fill(buf)          # object 0
/// stream.fill(buf)          # object 1 — different bytes
///
/// # Or allocate a new BytesView in one call
/// data = stream.generate(8 * 1024 * 1024)
/// view = memoryview(data)   # zero-copy access
/// ```
#[pyclass(name = "XorStream")]
pub struct PyXorStream {
    inner: UniqueXorStream,
}

#[pymethods]
impl PyXorStream {
    /// Create a new `XorStream`.
    ///
    /// Seeds the 1 MiB base buffer from system entropy (wall clock + `getrandom`).
    /// Every call to `XorStream()` produces a distinct instance with its own
    /// unique base.
    #[new]
    fn new() -> Self {
        Self {
            inner: UniqueXorStream::new(),
        }
    }

    /// Fill a pre-allocated buffer with unique, dedup-safe data.
    ///
    /// This is the fastest path for storage benchmarks: allocate once with
    /// `bytearray(size)`, then call `fill()` in a tight loop — no per-call
    /// heap allocation.
    ///
    /// # Arguments
    /// * `buffer` — Any writable Python buffer: `bytearray`, `memoryview`, numpy array, etc.
    ///
    /// # Example
    /// ```python
    /// stream = dgen_py.XorStream()
    /// buf = bytearray(8 * 1024 * 1024)
    ///
    /// for i in range(1000):
    ///     stream.fill(buf)
    ///     # send buf to your storage client here
    /// ```
    fn fill(&self, py: Python<'_>, buffer: &Bound<'_, PyAny>) -> PyResult<()> {
        let buf: PyBuffer<u8> = PyBuffer::get(buffer)?;
        if buf.readonly() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "XorStream.fill() requires a writable buffer (bytearray, numpy array, etc.)",
            ));
        }
        if !buf.is_c_contiguous() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Buffer must be C-contiguous",
            ));
        }
        let size = buf.len_bytes();
        // Release GIL while generating — allows other Python threads to proceed
        py.detach(|| unsafe {
            let dst_ptr = buf.buf_ptr() as *mut u8;
            let dst_slice = std::slice::from_raw_parts_mut(dst_ptr, size);
            self.inner.fill(dst_slice);
        });
        Ok(())
    }

    /// Allocate a new `BytesView` of `size` bytes, filled with unique data.
    ///
    /// Convenient when you do not want to manage a pre-allocated buffer.
    /// For tight loops, prefer `fill()` with a reused `bytearray` to avoid
    /// the per-call heap allocation.
    ///
    /// Returns a `BytesView` that supports the Python buffer protocol:
    /// - `memoryview(data)` — zero-copy view
    /// - `bytes(data)` — copies to a `bytes` object
    /// - `numpy.frombuffer(memoryview(data))` — zero-copy numpy array
    ///
    /// # Example
    /// ```python
    /// stream = dgen_py.XorStream()
    ///
    /// data = stream.generate(8 * 1024 * 1024)
    /// view = memoryview(data)        # zero-copy access to raw bytes
    /// raw  = bytes(data)             # copies to Python bytes object
    ///
    /// import numpy as np
    /// arr = np.frombuffer(view, dtype=np.uint8)  # zero-copy numpy view
    /// ```
    fn generate(&self, py: Python<'_>, size: usize) -> PyResult<Py<PyBytesView>> {
        let mut buf = vec![0u8; size];
        // Release GIL while filling — allows other Python threads to run
        py.detach(|| {
            self.inner.fill(&mut buf);
        });
        Py::new(
            py,
            PyBytesView {
                inner: PyBytesViewInner::Owned(DataBuffer::Uma(buf)),
            },
        )
    }

    /// Number of objects generated by this instance (fill + generate calls combined).
    ///
    /// Each call to `fill()` or `generate()` increments this counter by one.
    /// Useful for diagnostics and progress tracking.
    #[getter]
    fn objects_generated(&self) -> u64 {
        self.inner.objects_generated()
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

    // Rolling pool explicit API
    m.add_class::<PyBufferPool>()?;

    // XOR stream: fast dedup-safe generation without Rayon
    m.add_class::<PyXorStream>()?;

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
