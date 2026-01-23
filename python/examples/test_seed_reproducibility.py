#!/usr/bin/env python3
"""
Test script to demonstrate seed parameter for reproducible data generation.

This script shows:
1. With same seed → identical data (reproducible)
2. Without seed (None) → different data each time (non-deterministic)
"""

import dgen_py
import hashlib

def hash_buffer(buf):
    """Calculate SHA256 hash of buffer for comparison"""
    return hashlib.sha256(bytes(buf)).hexdigest()

def test_with_seed():
    """Test reproducibility with fixed seed"""
    print("=" * 80)
    print("TEST 1: Reproducibility with seed=12345")
    print("=" * 80)
    
    size = 10 * 1024 * 1024  # 10 MB
    seed = 12345
    
    # Generate data twice with same seed
    gen1 = dgen_py.Generator(size=size, seed=seed)
    buffer1 = bytearray(gen1.chunk_size)
    nbytes1 = gen1.fill_chunk(buffer1)
    hash1 = hash_buffer(buffer1[:nbytes1])
    
    gen2 = dgen_py.Generator(size=size, seed=seed)
    buffer2 = bytearray(gen2.chunk_size)
    nbytes2 = gen2.fill_chunk(buffer2)
    hash2 = hash_buffer(buffer2[:nbytes2])
    
    print(f"First generation  (seed={seed}): {hash1}")
    print(f"Second generation (seed={seed}): {hash2}")
    print(f"Hashes match: {hash1 == hash2}")
    print(f"✅ PASS: Data is reproducible with same seed\n" if hash1 == hash2 
          else f"❌ FAIL: Data should be identical\n")
    
    return hash1 == hash2

def test_without_seed():
    """Test non-determinism without seed (default behavior)"""
    print("=" * 80)
    print("TEST 2: Non-determinism without seed (seed=None, default)")
    print("=" * 80)
    
    size = 10 * 1024 * 1024  # 10 MB
    
    # Generate data twice without seed (entropy-based)
    gen1 = dgen_py.Generator(size=size)  # seed=None (default)
    buffer1 = bytearray(gen1.chunk_size)
    nbytes1 = gen1.fill_chunk(buffer1)
    hash1 = hash_buffer(buffer1[:nbytes1])
    
    gen2 = dgen_py.Generator(size=size)  # seed=None (default)
    buffer2 = bytearray(gen2.chunk_size)
    nbytes2 = gen2.fill_chunk(buffer2)
    hash2 = hash_buffer(buffer2[:nbytes2])
    
    print(f"First generation  (seed=None): {hash1}")
    print(f"Second generation (seed=None): {hash2}")
    print(f"Hashes differ: {hash1 != hash2}")
    print(f"✅ PASS: Data is non-deterministic (uses time+urandom)\n" if hash1 != hash2 
          else f"⚠️  WARNING: Hashes matched by coincidence (extremely unlikely)\n")
    
    return hash1 != hash2

def test_different_seeds():
    """Test that different seeds produce different data"""
    print("=" * 80)
    print("TEST 3: Different seeds produce different data")
    print("=" * 80)
    
    size = 10 * 1024 * 1024  # 10 MB
    
    gen1 = dgen_py.Generator(size=size, seed=11111)
    buffer1 = bytearray(gen1.chunk_size)
    nbytes1 = gen1.fill_chunk(buffer1)
    hash1 = hash_buffer(buffer1[:nbytes1])
    
    gen2 = dgen_py.Generator(size=size, seed=22222)
    buffer2 = bytearray(gen2.chunk_size)
    nbytes2 = gen2.fill_chunk(buffer2)
    hash2 = hash_buffer(buffer2[:nbytes2])
    
    print(f"Generation with seed=11111: {hash1}")
    print(f"Generation with seed=22222: {hash2}")
    print(f"Hashes differ: {hash1 != hash2}")
    print(f"✅ PASS: Different seeds produce different data\n" if hash1 != hash2 
          else f"❌ FAIL: Different seeds should produce different data\n")
    
    return hash1 != hash2

if __name__ == "__main__":
    print("\n")
    print("dgen-py Seed Parameter Test Suite")
    print("=" * 80)
    print()
    
    results = []
    results.append(("Reproducibility with seed", test_with_seed()))
    results.append(("Non-determinism without seed", test_without_seed()))
    results.append(("Different seeds → different data", test_different_seeds()))
    
    print("=" * 80)
    print("SUMMARY")
    print("=" * 80)
    for name, passed in results:
        status = "✅ PASS" if passed else "❌ FAIL"
        print(f"{status}: {name}")
    
    all_passed = all(r[1] for r in results)
    print()
    if all_passed:
        print("🎉 All tests passed!")
    else:
        print("⚠️  Some tests failed")
    
    exit(0 if all_passed else 1)
