"""
dgen-py: High-performance random data generation with NUMA optimization

TRUE ZERO-COPY: Uses Python buffer protocol for zero-copy access to generated data.
No memcpy between Rust and Python - same performance as numpy!
"""

from typing import Optional
import sys

# Get version from package metadata (automatically syncs with pyproject.toml)
try:
    from importlib.metadata import version
    __version__ = version("dgen-py")
except Exception:
    # Fallback for development/editable installs
    __version__ = "unknown"

# Import Rust extension module
try:
    from ._dgen_rs import (
        BytesView,
        generate_buffer,
        generate_into_buffer,
        Generator,
        create_bytearrays,
    )
    
    # Try to import NUMA info (may not be available on all platforms)
    try:
        from ._dgen_rs import get_numa_info
    except ImportError:
        get_numa_info = None
        
except ImportError as e:
    raise ImportError(
        f"Failed to import dgen-py Rust extension: {e}\n"
        "Please ensure the package is properly installed:\n"
        "  pip install dgen-py\n"
        "Or build from source:\n"
        "  cd dgen-rs && maturin develop --release"
    )

__all__ = [
    "BytesView",
    "generate_buffer",
    "generate_data",
    "generate_into_buffer",
    "fill_buffer",
    "Generator",
    "create_bytearrays",
    "get_numa_info",
    "get_system_info",
]


def generate_data(
    size: int,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0,
    numa_mode: str = "auto",
    max_threads: Optional[int] = None,
):
    """
    Generate random data with ZERO-COPY access via buffer protocol.
    
    Returns a BytesView object that supports memoryview() for true zero-copy access.
    No memcpy between Rust and Python - same memory is shared!
    
    Args:
        size: Total bytes to generate
        dedup_ratio: Deduplication ratio (1.0 = no dedup, 2.0 = 2:1 ratio)
        compress_ratio: Compression ratio (1.0 = incompressible, 3.0 = 3:1 ratio)
        numa_mode: NUMA optimization - \"auto\" (default), \"force\", or \"disabled\"
        max_threads: Maximum threads to use (None = use all cores)
    
    Returns:
        BytesView: Zero-copy buffer (use memoryview() or numpy.frombuffer() for access)
    
    Example - Zero-copy with numpy (fastest):
        >>> import dgen_py
        >>> import numpy as np
        >>> 
        >>> # Generate data (no copy)
        >>> data = dgen_py.generate_data(1024 * 1024)
        >>> 
        >>> # Create memoryview (no copy)
        >>> view = memoryview(data)
        >>> 
        >>> # Create numpy array (STILL no copy!)
        >>> arr = np.frombuffer(view, dtype=np.uint8)
        >>> 
        >>> # All three share the SAME memory - zero copies!
        >>> len(data), len(view), len(arr)
        (1048576, 1048576, 1048576)
        
    Example - Get Python bytes (copies data):
        >>> # If you need actual bytes object, call bytes()
        >>> data_bytes = bytes(data)  # This copies, but gives you bytes object
    """
    return generate_buffer(size, dedup_ratio, compress_ratio, numa_mode, max_threads)


def fill_buffer(
    buffer,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0,
    numa_mode: str = "auto",
    max_threads: Optional[int] = None,
) -> int:
    """
    Generate data directly into an existing buffer (zero-copy).
    
    This is the most efficient API for pre-allocated buffers.
    Works with bytearray, memoryview, numpy arrays, etc.
    
    Args:
        buffer: Pre-allocated writable buffer (supports buffer protocol)
        dedup_ratio: Deduplication ratio
        compress_ratio: Compression ratio
        numa_mode: NUMA optimization - "auto" (default), "force", or "disabled"
        max_threads: Maximum threads to use (None = use all cores)
    
    Returns:
        int: Number of bytes written
    
    Example:
        >>> import dgen_py
        >>> 
        >>> # Pre-allocate buffer
        >>> buf = bytearray(1024 * 1024)
        >>> 
        >>> # Generate directly into buffer (zero-copy) using 4 threads
        >>> nbytes = dgen_py.fill_buffer(buf, compress_ratio=2.0, max_threads=4)
        >>> print(f"Wrote {nbytes} bytes")
        
        >>> # Works with numpy arrays
        >>> import numpy as np
        >>> arr = np.zeros(1024 * 1024, dtype=np.uint8)
        >>> nbytes = dgen_py.fill_buffer(arr, dedup_ratio=2.0)
    """
    return generate_into_buffer(buffer, dedup_ratio, compress_ratio, numa_mode, max_threads)


def get_system_info() -> Optional[dict]:
    """
    Get NUMA topology information (if available).
    
    Returns:
        dict: NUMA info with keys:
            - num_nodes: Number of NUMA nodes
            - physical_cores: Total physical cores
            - logical_cpus: Total logical CPUs
            - is_uma: Whether this is a UMA system
            - deployment_type: Description of deployment type
        None: If NUMA detection is not available on this platform
    
    Example:
        >>> info = dgen_py.get_system_info()
        >>> if info:
        ...     print(f"NUMA nodes: {info['num_nodes']}")
        ...     print(f"Cores: {info['physical_cores']}")
        ...     print(f"Type: {info['deployment_type']}")
    """
    if get_numa_info is None:
        return None
    
    try:
        return get_numa_info()
    except Exception:
        return None


# Convenience alias
StreamingGenerator = Generator

