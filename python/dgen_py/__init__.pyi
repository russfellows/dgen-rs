"""Type stubs for dgen-py v0.2.4"""

from typing import Optional

# ---------------------------------------------------------------------------
# Low-level buffer type (zero-copy, supports Python buffer protocol)
# ---------------------------------------------------------------------------

class BytesView:
    """Zero-copy buffer returned by generate_buffer() / XorStream.generate().

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
# XorStream — fast, dedup-safe generation without Rayon (new in v0.2.4)
# ---------------------------------------------------------------------------

class XorStream:
    """Fast, dedup-safe data generator using XOR keystream (new in v0.2.4).

    Holds a 1 MiB random base buffer and an atomic counter.  Each fill() or
    generate() call produces a unique output — no two calls share a 512-byte
    fingerprint.  Thread-safe: &self methods, no mutex required.

    Performance: ~15 GB/s per core.  No Rayon, no per-call allocation on the
    fill() path.

    Example::

        import dgen_py
        stream = dgen_py.XorStream()

        # Fastest: fill pre-allocated bytearray in-place
        buf = bytearray(8 * 1024 * 1024)
        stream.fill(buf)            # object 0
        stream.fill(buf)            # object 1 — different bytes, guaranteed

        # Convenience: allocate + fill in one call
        data = stream.generate(8 * 1024 * 1024)
        view = memoryview(data)     # zero-copy
    """

    def __init__(self) -> None: ...

    def fill(self, buffer: object) -> None:
        """Fill a pre-allocated writable buffer with unique, dedup-safe data.

        GIL is released during generation.

        Args:
            buffer: bytearray, memoryview, numpy uint8 array, etc.

        Raises:
            ValueError: if buffer is read-only or not C-contiguous.
        """
        ...

    def generate(self, size: int) -> BytesView:
        """Allocate a new BytesView of size bytes, filled with unique data."""
        ...

    @property
    def objects_generated(self) -> int:
        """Total fill() + generate() calls on this instance."""
        ...

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
