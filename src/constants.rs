// src/constants.rs
//
// SPDX-License-Identifier: MIT OR Apache-2.0

/// Default block size for data generation (4 MiB)
/// This matches typical cloud storage object sizes and provides good parallelism
pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// Minimum size for data generation (one block)
pub const MIN_SIZE: usize = BLOCK_SIZE;

/// Maximum back-reference distance for compression (1 KiB)
/// Keeps compression patterns local for realistic compressor behavior
pub const MAX_BACK_REF_DISTANCE: usize = 1024;

/// Minimum run length for back-references (64 bytes)
pub const MIN_RUN_LENGTH: usize = 64;

/// Maximum run length for back-references (256 bytes)
pub const MAX_RUN_LENGTH: usize = 256;
