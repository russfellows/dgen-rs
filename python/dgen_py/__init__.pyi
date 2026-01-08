"""Type stubs for dgen-py"""

from typing import Optional

def generate_buffer(
    size: int,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0
) -> bytes:
    """Generate random data with controllable characteristics"""
    ...

def generate_into_buffer(
    buffer,
    dedup_ratio: float = 1.0,
    compress_ratio: float = 1.0
) -> int:
    """Generate data directly into existing buffer (zero-copy)"""
    ...

class Generator:
    """Streaming data generator"""
    
    def __init__(
        self,
        size: int,
        dedup_ratio: float = 1.0,
        compress_ratio: float = 1.0,
        numa_mode: str = "auto",
        max_threads: Optional[int] = None
    ) -> None:
        """Create new generator"""
        ...
    
    def fill_chunk(self, buffer) -> int:
        """Fill next chunk into buffer"""
        ...
    
    def get_chunk(self, chunk_size: int) -> Optional[bytes]:
        """Get next chunk as bytes"""
        ...
    
    def reset(self) -> None:
        """Reset to start"""
        ...
    
    def position(self) -> int:
        """Get current position"""
        ...
    
    def total_size(self) -> int:
        """Get total size"""
        ...
    
    def is_complete(self) -> bool:
        """Check if complete"""
        ...

def get_numa_info() -> dict:
    """Get NUMA topology information"""
    ...
