"""Type stubs for dgen-py v0.2.4"""

from typing import Optional

# ---------------------------------------------------------------------------
# Low-level buffer type (zero-copy, supports Python buffer protocol)
# ---------------------------------------------------------------------------

class BytesView:
    """Zero-copy buffer returned by generate_buffer() and similar functions.

    Supports the Python buffer protocol — use memoryview() for zero-copy
    access, or bytes() to copy to a Python bytes object.
    """
    def __len__(self) -> int: ...
    def __bytes__(self) -> bytes: ...

# ---------------------------------------------------------------------------
# Simple one-shot generation functions
# ---------------------------------------------------------------------------

def generate_buffer(
    size: int,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0,
    numa_mode: str = "auto",
    max_threads: Optional[int] = None,
    numa_node: Optional[int] = None,
) -> BytesView: ...

def generate_into_buffer(
    buffer: object,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0,
    numa_mode: str = "auto",
    max_threads: Optional[int] = None,
    numa_node: Optional[int] = None,
) -> int: ...

def create_bytearrays(count: int, size: int) -> list: ...

def bench_rolling_pool(obj_size: int, total_bytes: int) -> tuple[int, float]: ...

# ---------------------------------------------------------------------------
# Numeric distributions (docs/DESIGN_NUMERIC_DISTRIBUTIONS.md, storage#625)
# ---------------------------------------------------------------------------

def generate_uniform(
    count: int,
    low: float = 0.0,
    high: float = 1.0,
    max_threads: Optional[int] = None,
    numa_mode: str = "auto",
    seed: Optional[int] = None,
) -> BytesView: ...

def normalize_rows(
    buffer: object,
    dim: int,
    max_threads: Optional[int] = None,
) -> None: ...

def generate_uniform_vectors(
    rows: int,
    dim: int,
    low: float = 0.0,
    high: float = 1.0,
    normalize: bool = True,
    max_threads: Optional[int] = None,
    numa_mode: str = "auto",
    seed: Optional[int] = None,
) -> BytesView: ...

# ---------------------------------------------------------------------------
# Rolling pool (explicit, for tight loops)
# ---------------------------------------------------------------------------

class BufferPool:
    """Explicit rolling buffer pool for high-frequency small-object workloads."""

    def __init__(
        self,
        dedup_ratio: float = 1.0,
        compress_ratio: float = 1.0,
    ) -> None: ...

    def next_slice(self, size: int) -> BytesView: ...

    def reconfigure(
        self,
        dedup_ratio: float = 1.0,
        compress_ratio: float = 1.0,
    ) -> None: ...

    @property
    def remaining(self) -> int: ...

    @property
    def dedup_ratio(self) -> int: ...

    @property
    def compress_ratio(self) -> int: ...

# ---------------------------------------------------------------------------
# Streaming generator
# ---------------------------------------------------------------------------

class Generator:
    """Streaming data generator — unlimited data with constant memory usage."""

    def __init__(
        self,
        size: int,
        dedup_ratio: float = 1.0,
        compress_ratio: float = 1.0,
        numa_mode: str = "auto",
        max_threads: Optional[int] = None,
        numa_node: Optional[int] = None,
        chunk_size: Optional[int] = None,
        block_size: Optional[int] = None,
        seed: Optional[int] = None,
    ) -> None: ...

    @property
    def chunk_size(self) -> int: ...

    def fill_chunk(self, buffer: object) -> int: ...

    def get_chunk(self, chunk_size: int) -> Optional[BytesView]: ...

    def reset(self) -> None: ...

    def position(self) -> int: ...

    def total_size(self) -> int: ...

    def is_complete(self) -> bool: ...

    def set_seed(self, seed: Optional[int]) -> None: ...

# ---------------------------------------------------------------------------
# NUMA topology
# ---------------------------------------------------------------------------

def get_numa_info() -> Optional[dict]: ...
