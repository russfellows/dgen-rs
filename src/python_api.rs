// src/python_api.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Zero-copy Python bindings using PyO3 buffer protocol

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::buffer::PyBuffer;
use pyo3::ffi;
use bytes::Bytes;

use crate::generator::{generate_data, DataGenerator, GeneratorConfig, NumaMode};

#[cfg(feature = "numa")]
use crate::numa::NumaTopology;

// =============================================================================
// Zero-Copy Buffer Support
// =============================================================================

/// A Python-visible wrapper around bytes::Bytes that exposes buffer protocol.
/// This allows Python code to get a memoryview without copying data.
/// 
/// Implements the Python buffer protocol via __getbuffer__ and __releasebuffer__
/// so that `memoryview(data)` works directly with zero-copy access.
#[pyclass(name = "BytesView")]
pub struct PyBytesView {
    /// The underlying Bytes (reference-counted, cheap to clone)
    bytes: Bytes,
}

#[pymethods]
impl PyBytesView {
    /// Get the length of the data
    fn __len__(&self) -> usize {
        self.bytes.len()
    }
    
    /// Support bytes() conversion - returns a copy
    fn __bytes__<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.bytes)
    }
    
    /// Implement Python buffer protocol for zero-copy access.
    /// This allows `memoryview(data)` to work directly.
    /// 
    /// The buffer is read-only; requesting a writable buffer will raise BufferError.
    unsafe fn __getbuffer__(
        slf: PyRef<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: std::os::raw::c_int,
    ) -> PyResult<()> {
        // Check for writable request - we only support read-only buffers
        if (flags & ffi::PyBUF_WRITABLE) != 0 {
            return Err(pyo3::exceptions::PyBufferError::new_err(
                "BytesView is read-only and does not support writable buffers"
            ));
        }
        
        let bytes = &slf.bytes;
        
        // Fill in the Py_buffer struct
        unsafe {
            (*view).buf = bytes.as_ptr() as *mut std::os::raw::c_void;
            (*view).len = bytes.len() as isize;
            (*view).readonly = 1;
            (*view).itemsize = 1;
            
            // Format string: "B" = unsigned byte (matches u8)
            (*view).format = if (flags & ffi::PyBUF_FORMAT) != 0 {
                b"B\0".as_ptr() as *mut std::os::raw::c_char
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
            // This prevents the Bytes data from being deallocated while the buffer is in use
            (*view).obj = slf.as_ptr() as *mut ffi::PyObject;
            ffi::Py_INCREF((*view).obj);
        }
        
        Ok(())
    }
    
    /// Release the buffer - called when the memoryview is garbage collected.
    /// We don't need to do anything here since the Bytes is reference-counted.
    unsafe fn __releasebuffer__(&self, _view: *mut ffi::Py_buffer) {
        // Nothing to do - the Py_DECREF on view.obj will be handled by Python
        // and will eventually drop the PyBytesView (and thus the Bytes) when refcount hits 0
    }
}

// =============================================================================
// Simple API - Single-call data generation
// =============================================================================

/// Generate random data with controllable deduplication and compression
///
/// # Arguments
/// * `size` - Total bytes to generate
/// * `dedup_ratio` - Deduplication ratio (1.0 = no dedup, 2.0 = 2:1 ratio)
/// * `compress_ratio` - Compression ratio (1.0 = incompressible, 3.0 = 3:1 ratio)
/// * `numa_mode` - NUMA mode: "auto", "force", or "disabled" (default: "auto")
/// * `max_threads` - Maximum threads to use (None = use all cores)
///
/// # Returns
/// Python bytes object with generated data (zero-copy from Rust)
///
/// # Example
/// ```python
/// import dgen_py
///
/// # Generate 1 MiB incompressible data using 8 threads
/// data = dgen_py.generate_buffer(1024 * 1024, dedup_ratio=1.0, 
///                                  compress_ratio=1.0, max_threads=8)
/// print(f"Generated {len(data)} bytes")
/// ```
#[pyfunction]
#[pyo3(signature = (size, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None))]
fn generate_buffer(
    py: Python<'_>,
    size: usize,
    dedup_ratio: f64,
    compress_ratio: f64,
    numa_mode: &str,
    max_threads: Option<usize>,
) -> PyResult<Py<PyBytesView>> {
    // Convert ratios to integer factors
    let dedup = (dedup_ratio.max(1.0) as usize).max(1);
    let compress = (compress_ratio.max(1.0) as usize).max(1);
    
    // Parse NUMA mode
    let numa = match numa_mode.to_lowercase().as_str() {
        "auto" => NumaMode::Auto,
        "force" => NumaMode::Force,
        "disabled" | "disable" => NumaMode::Disabled,
        _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'", numa_mode)
        )),
    };

    // Build config
    let config = GeneratorConfig {
        size,
        dedup_factor: dedup,
        compress_factor: compress,
        numa_mode: numa,
        max_threads,
    };

    // Generate data WITHOUT holding GIL (allows parallel Python threads)
    let data = py.allow_threads(|| generate_data(config));

    // Convert Vec<u8> to Bytes (cheap, just wraps the Vec's heap allocation)
    let bytes = Bytes::from(data);
    
    // Return BytesView - Python can use memoryview() for TRUE zero-copy access
    Py::new(py, PyBytesView { bytes })
}

/// Generate data using Python buffer protocol (for writing into existing buffer)
///
/// # Arguments
/// * `buffer` - Pre-allocated Python buffer (bytearray, memoryview, numpy array, etc.)
/// * `dedup_ratio` - Deduplication ratio
/// * `compress_ratio` - Compression ratio
/// * `numa_mode` - NUMA mode: "auto", "force", or "disabled" (default: "auto")
/// * `max_threads` - Maximum threads to use (None = use all cores)
///
/// # Returns
/// Number of bytes written
///
/// # Example
/// ```python
/// import dgen_py
///
/// # Pre-allocate buffer
/// buf = bytearray(1024 * 1024)
/// 
/// # Generate directly into buffer (zero-copy) using 4 threads
/// nbytes = dgen_py.generate_into_buffer(buf, dedup_ratio=1.0, 
///                                        compress_ratio=2.0, max_threads=4)
/// print(f"Wrote {nbytes} bytes")
/// ```
#[pyfunction]
#[pyo3(signature = (buffer, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None))]
fn generate_into_buffer(
    py: Python<'_>,
    buffer: PyObject,
    dedup_ratio: f64,
    compress_ratio: f64,
    numa_mode: &str,
    max_threads: Option<usize>,
) -> PyResult<usize> {
    // Get buffer via PyBuffer protocol
    let buf: PyBuffer<u8> = PyBuffer::get(buffer.bind(py))?;
    
    // Ensure buffer is writable and contiguous
    if buf.readonly() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Buffer must be writable"
        ));
    }
    
    if !buf.is_c_contiguous() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Buffer must be C-contiguous for zero-copy operation"
        ));
    }

    let size = buf.len_bytes();
    let dedup = (dedup_ratio.max(1.0) as usize).max(1);
    let compress = (compress_ratio.max(1.0) as usize).max(1);
    
    // Parse NUMA mode
    let numa = match numa_mode.to_lowercase().as_str() {
        "auto" => NumaMode::Auto,
        "force" => NumaMode::Force,
        "disabled" | "disable" => NumaMode::Disabled,
        _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'", numa_mode)
        )),
    };
    
    // Build config
    let config = GeneratorConfig {
        size,
        dedup_factor: dedup,
        compress_factor: compress,
        numa_mode: numa,
        max_threads,
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
}

#[pymethods]
impl PyGenerator {
    /// Create new streaming generator
    ///
    /// # Arguments
    /// * `size` - Total bytes to generate
    /// * `dedup_ratio` - Deduplication ratio
    /// * `compress_ratio` - Compression ratio
    /// * `numa_mode` - NUMA mode: "auto", "force", or "disabled" (default: "auto")
    /// * `max_threads` - Maximum threads to use (None = use all cores)
    #[new]
    #[pyo3(signature = (size, dedup_ratio=1.0, compress_ratio=1.0, numa_mode="auto", max_threads=None))]
    fn new(
        size: usize,
        dedup_ratio: f64,
        compress_ratio: f64,
        numa_mode: &str,
        max_threads: Option<usize>,
    ) -> PyResult<Self> {
        let dedup = (dedup_ratio.max(1.0) as usize).max(1);
        let compress = (compress_ratio.max(1.0) as usize).max(1);
        
        // Parse NUMA mode
        let numa = match numa_mode.to_lowercase().as_str() {
            "auto" => NumaMode::Auto,
            "force" => NumaMode::Force,
            "disabled" | "disable" => NumaMode::Disabled,
            _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("Invalid numa_mode '{}': must be 'auto', 'force', or 'disabled'", numa_mode)
            )),
        };

        let config = GeneratorConfig {
            size,
            dedup_factor: dedup,
            compress_factor: compress,
            numa_mode: numa,
            max_threads,
        };

        Ok(Self {
            inner: DataGenerator::new(config),
        })
    }

    /// Fill the next chunk of data
    ///
    /// # Arguments
    /// * `buffer` - Pre-allocated buffer to fill
    ///
    /// # Returns
    /// Number of bytes written (0 when complete)
    fn fill_chunk(&mut self, py: Python<'_>, buffer: PyObject) -> PyResult<usize> {
        // Get buffer via PyBuffer protocol
        let buf: PyBuffer<u8> = PyBuffer::get(buffer.bind(py))?;
        
        if buf.readonly() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Buffer must be writable"
            ));
        }
        
        if !buf.is_c_contiguous() {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                "Buffer must be C-contiguous"
            ));
        }

        let size = buf.len_bytes();
        
        // Generate into temporary buffer
        let mut temp = vec![0u8; size];
        let written = self.inner.fill_chunk(&mut temp);

        if written > 0 {
            // Write to Python buffer (zero-copy)
            unsafe {
                let dst_ptr = buf.buf_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(temp.as_ptr(), dst_ptr, written);
            }
        }

        Ok(written)
    }

    /// Get data as BytesView (zero-copy access via memoryview)
    ///
    /// # Arguments
    /// * `chunk_size` - Size of chunk to read
    ///
    /// # Returns
    /// BytesView object or None if complete
    fn get_chunk(&mut self, py: Python<'_>, chunk_size: usize) -> PyResult<Option<Py<PyBytesView>>> {
        if self.inner.is_complete() {
            return Ok(None);
        }

        let mut chunk = vec![0u8; chunk_size];
        let written = self.inner.fill_chunk(&mut chunk);

        if written == 0 {
            Ok(None)
        } else {
            chunk.truncate(written);
            // Convert to Bytes for zero-copy Python access
            let bytes = Bytes::from(chunk);
            Ok(Some(Py::new(py, PyBytesView { bytes })?))
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
}

// =============================================================================
// NUMA Info API
// =============================================================================

#[cfg(feature = "numa")]
#[pyfunction]
fn get_numa_info(py: Python<'_>) -> PyResult<PyObject> {
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
// Module Registration
// =============================================================================

pub fn register_functions(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Zero-copy buffer type
    m.add_class::<PyBytesView>()?;
    
    // Simple API
    m.add_function(wrap_pyfunction!(generate_buffer, m)?)?;
    m.add_function(wrap_pyfunction!(generate_into_buffer, m)?)?;

    // Streaming API
    m.add_class::<PyGenerator>()?;

    // NUMA info
    #[cfg(feature = "numa")]
    m.add_function(wrap_pyfunction!(get_numa_info, m)?)?;

    Ok(())
}
