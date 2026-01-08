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

// Core modules
pub mod constants;
pub mod generator;

#[cfg(feature = "numa")]
pub mod numa;

// Python bindings
#[cfg(feature = "python-bindings")]
mod python_api;

// Re-export main API
pub use generator::{
    generate_data, generate_data_simple, DataGenerator, GeneratorConfig, NumaMode,
};

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
