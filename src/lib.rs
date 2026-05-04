// src/lib.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! High-performance random data generation with controllable deduplication and compression
//!
//! This library provides:
//! - Xoshiro256++ RNG for high-speed data generation (5-15 GB/s per core)
//! - Controllable deduplication ratios (1:1 to N:1)
//! - Controllable compression ratios (1:1 to N:1)
//! - NUMA-aware parallel generation (optional)
//! - Zero-copy Python bindings via PyO3
//!
//! # Quick-start for async HTTP servers
//!
//! The easiest way to stream fake data from a GET handler is via the
//! [`thread_local`] module.  One pool per worker thread, zero-copy for chunks
//! ≤ 1 MiB, no re-creation, no re-seeding between requests:
//!
//! ```rust
//! use bytes::Bytes;
//! use dgen_data::thread_local::next_slice;
//!
//! // Inside a stream::unfold closure (before any .await):
//! fn get_chunk(chunk_size: usize) -> Bytes {
//!     next_slice(chunk_size)
//! }
//! ```

// Core modules
pub mod constants;
pub mod generator;
pub mod rolling_pool;
pub mod xor_stream;

/// Thread-local rolling pool — zero-overhead data generation for async servers.
///
/// See the [module documentation](thread_local) for full design notes.
pub mod thread_local;

#[cfg(feature = "numa")]
pub mod numa;

// Python bindings
#[cfg(feature = "python-bindings")]
mod python_api;

// Re-export main API
pub use generator::{
    generate_data, generate_data_simple, DataGenerator, GeneratorConfig, NumaMode,
};

// Re-export rolling pool (additive; does not change any existing API)
pub use rolling_pool::RollingPool;

// Re-export XOR stream (fast, dedup-safe generation without Rayon)
pub use xor_stream::UniqueXorStream;

// Re-export BLOCK_SIZE so callers can choose optimal chunk sizes without
// having to import the constants module directly.
pub use constants::BLOCK_SIZE;

#[cfg(feature = "numa")]
pub use numa::{NumaNode, NumaTopology};

// PyO3 module initialization
#[cfg(feature = "python-bindings")]
use pyo3::prelude::*;

#[cfg(feature = "python-bindings")]
#[pymodule]
fn _dgen_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register all Python functions
    python_api::register_functions(m)?;
    Ok(())
}
