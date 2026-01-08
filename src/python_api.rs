// src/python_api.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Zero-copy Python bindings using PyO3

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::buffer::PyBuffer;

use crate::generator::{generate_data, DataGenerator, GeneratorConfig, NumaMode};

#[cfg(feature = "numa")]
use crate::numa::NumaTopology;

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
) -> PyResult<Py<PyBytes>> {
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

    // Generate data
    let data = generate_data(config);

    // Return as PyBytes (zero-copy via Into<Py<PyBytes>>)
    // PyO3 transfers ownership of Vec<u8> to Python, avoiding copy
    Ok(PyBytes::new(py, &data).into())
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

    /// Get data as bytes (convenience method, allocates new buffer)
    ///
    /// # Arguments
    /// * `chunk_size` - Size of chunk to read
    ///
    /// # Returns
    /// Python bytes object or None if complete
    fn get_chunk(&mut self, py: Python<'_>, chunk_size: usize) -> PyResult<Option<Py<PyBytes>>> {
        if self.inner.is_complete() {
            return Ok(None);
        }

        let mut chunk = vec![0u8; chunk_size];
        let written = self.inner.fill_chunk(&mut chunk);

        if written == 0 {
            Ok(None)
        } else {
            chunk.truncate(written);
            Ok(Some(PyBytes::new(py, &chunk).into()))
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
