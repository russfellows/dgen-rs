"""Tests for dgen-py Python bindings"""

import pytest
import dgen_py


def test_simple_generation():
    """Test simple API generates correct size"""
    size = 1024 * 1024  # 1 MiB
    data = dgen_py.generate_data(size)
    
    assert isinstance(data, bytes)
    # Size may be rounded up to block size
    assert len(data) >= size


def test_dedup_ratio():
    """Test deduplication ratio"""
    size = 10 * 1024 * 1024  # 10 MiB
    data = dgen_py.generate_data(size, dedup_ratio=2.0)
    
    assert isinstance(data, bytes)
    assert len(data) >= size


def test_compress_ratio():
    """Test compression ratio"""
    size = 10 * 1024 * 1024  # 10 MiB
    data = dgen_py.generate_data(size, compress_ratio=3.0)
    
    assert isinstance(data, bytes)
    assert len(data) >= size


def test_zero_copy_into_buffer():
    """Test zero-copy generation into pre-allocated buffer"""
    size = 1024 * 1024  # 1 MiB
    buf = bytearray(size)
    
    nbytes = dgen_py.fill_buffer(buf, compress_ratio=2.0)
    
    assert nbytes == size
    assert len(buf) == size


def test_streaming_generator():
    """Test streaming generator"""
    total_size = 10 * 1024 * 1024  # 10 MiB
    chunk_size = 8192
    
    gen = dgen_py.Generator(
        size=total_size,
        dedup_ratio=1.0,
        compress_ratio=1.0
    )
    
    assert gen.total_size() >= total_size
    assert gen.position() == 0
    assert not gen.is_complete()
    
    # Generate all data
    buf = bytearray(chunk_size)
    total_generated = 0
    
    while not gen.is_complete():
        nbytes = gen.fill_chunk(buf)
        if nbytes == 0:
            break
        total_generated += nbytes
    
    assert gen.is_complete()
    assert total_generated >= total_size


def test_streaming_get_chunk():
    """Test streaming generator get_chunk method"""
    total_size = 1024 * 1024  # 1 MiB
    chunk_size = 8192
    
    gen = dgen_py.Generator(size=total_size)
    
    chunks = []
    while not gen.is_complete():
        chunk = gen.get_chunk(chunk_size)
        if chunk is None:
            break
        chunks.append(chunk)
    
    total = sum(len(c) for c in chunks)
    assert total >= total_size


def test_generator_reset():
    """Test generator reset"""
    gen = dgen_py.Generator(size=1024 * 1024)
    
    # Generate some data
    buf = bytearray(8192)
    gen.fill_chunk(buf)
    
    assert gen.position() > 0
    
    # Reset
    gen.reset()
    
    assert gen.position() == 0
    assert not gen.is_complete()


def test_numpy_integration():
    """Test NumPy array integration (if numpy available)"""
    try:
        import numpy as np
    except ImportError:
        pytest.skip("NumPy not available")
    
    size = 1024 * 1024  # 1 MiB
    arr = np.zeros(size, dtype=np.uint8)
    
    nbytes = dgen_py.fill_buffer(arr, compress_ratio=2.0)
    
    assert nbytes == size
    assert arr.sum() > 0  # Should have non-zero data


def test_system_info():
    """Test NUMA system info"""
    info = dgen_py.get_system_info()
    
    # May be None on platforms without NUMA support
    if info is not None:
        assert 'num_nodes' in info
        assert 'physical_cores' in info
        assert 'logical_cpus' in info
        assert 'is_uma' in info
        assert 'deployment_type' in info
        
        assert info['num_nodes'] >= 1
        assert info['physical_cores'] >= 1
        assert info['logical_cpus'] >= info['physical_cores']


if __name__ == '__main__':
    pytest.main([__file__, '-v'])
