#!/usr/bin/env python3
"""
Test script for set_seed() method to demonstrate dynamic seed changing.

This script validates:
1. Changing seed mid-stream produces different data patterns
2. Same seed sequence produces identical data
3. Switching to non-deterministic mode works correctly
"""

import dgen_py
import hashlib

def hash_buffer(buf):
    """Calculate SHA256 hash of buffer for comparison"""
    return hashlib.sha256(bytes(buf)).hexdigest()

def test_seed_change_produces_different_data():
    """Test that changing seed mid-stream changes data pattern"""
    print("=" * 80)
    print("TEST 1: Changing seed mid-stream produces different data")
    print("=" * 80)
    
    size = 20 * 1024 * 1024  # 20 MB
    chunk_size = 10 * 1024 * 1024  # 10 MB chunks
    
    # First generator with seed changes
    gen1 = dgen_py.Generator(size=size, seed=11111)
    buffer1a = bytearray(chunk_size)
    nbytes1a = gen1.fill_chunk(buffer1a)
    hash1a = hash_buffer(buffer1a[:nbytes1a])
    
    gen1.set_seed(22222)  # Change seed mid-stream
    buffer1b = bytearray(chunk_size)
    nbytes1b = gen1.fill_chunk(buffer1b)
    hash1b = hash_buffer(buffer1b[:nbytes1b])
    
    # Second generator staying with first seed
    gen2 = dgen_py.Generator(size=size, seed=11111)
    buffer2a = bytearray(chunk_size)
    nbytes2a = gen2.fill_chunk(buffer2a)
    hash2a = hash_buffer(buffer2a[:nbytes2a])
    
    buffer2b = bytearray(chunk_size)
    nbytes2b = gen2.fill_chunk(buffer2b)  # Still using seed=11111
    hash2b = hash_buffer(buffer2b[:nbytes2b])
    
    print(f"Gen1 chunk 1 (seed=11111): {hash1a}")
    print(f"Gen2 chunk 1 (seed=11111): {hash2a}")
    print(f"First chunks match: {hash1a == hash2a}")
    print()
    print(f"Gen1 chunk 2 (seed=22222): {hash1b}")
    print(f"Gen2 chunk 2 (seed=11111): {hash2b}")
    print(f"Second chunks differ: {hash1b != hash2b}")
    print()
    
    passed = (hash1a == hash2a) and (hash1b != hash2b)
    print(f"{'✅ PASS' if passed else '❌ FAIL'}: Seed change produces different data\n")
    return passed

def test_same_seed_sequence_reproducible():
    """Test that same seed sequence produces identical results"""
    print("=" * 80)
    print("TEST 2: Same seed sequence produces identical data")
    print("=" * 80)
    
    size = 30 * 1024 * 1024  # 30 MB
    chunk_size = 10 * 1024 * 1024  # 10 MB chunks
    
    # First run with seed sequence: 111 -> 222 -> 333
    gen1 = dgen_py.Generator(size=size, seed=111)
    
    buffer1a = bytearray(chunk_size)
    gen1.fill_chunk(buffer1a)
    hash1a = hash_buffer(buffer1a)
    
    gen1.set_seed(222)
    buffer1b = bytearray(chunk_size)
    gen1.fill_chunk(buffer1b)
    hash1b = hash_buffer(buffer1b)
    
    gen1.set_seed(333)
    buffer1c = bytearray(chunk_size)
    gen1.fill_chunk(buffer1c)
    hash1c = hash_buffer(buffer1c)
    
    # Second run with same seed sequence
    gen2 = dgen_py.Generator(size=size, seed=111)
    
    buffer2a = bytearray(chunk_size)
    gen2.fill_chunk(buffer2a)
    hash2a = hash_buffer(buffer2a)
    
    gen2.set_seed(222)
    buffer2b = bytearray(chunk_size)
    gen2.fill_chunk(buffer2b)
    hash2b = hash_buffer(buffer2b)
    
    gen2.set_seed(333)
    buffer2c = bytearray(chunk_size)
    gen2.fill_chunk(buffer2c)
    hash2c = hash_buffer(buffer2c)
    
    print(f"Run 1 - Chunk 1 (seed=111): {hash1a}")
    print(f"Run 2 - Chunk 1 (seed=111): {hash2a}")
    print(f"Chunk 1 matches: {hash1a == hash2a}")
    print()
    print(f"Run 1 - Chunk 2 (seed=222): {hash1b}")
    print(f"Run 2 - Chunk 2 (seed=222): {hash2b}")
    print(f"Chunk 2 matches: {hash1b == hash2b}")
    print()
    print(f"Run 1 - Chunk 3 (seed=333): {hash1c}")
    print(f"Run 2 - Chunk 3 (seed=333): {hash2c}")
    print(f"Chunk 3 matches: {hash1c == hash2c}")
    print()
    
    passed = (hash1a == hash2a) and (hash1b == hash2b) and (hash1c == hash2c)
    print(f"{'✅ PASS' if passed else '❌ FAIL'}: Same seed sequence is reproducible\n")
    return passed

def test_switch_to_nondeterministic():
    """Test switching to non-deterministic mode mid-stream"""
    print("=" * 80)
    print("TEST 3: Switching to non-deterministic mode")
    print("=" * 80)
    
    size = 20 * 1024 * 1024  # 20 MB
    chunk_size = 10 * 1024 * 1024  # 10 MB chunks
    
    # First generator: deterministic then non-deterministic
    gen1 = dgen_py.Generator(size=size, seed=12345)
    buffer1a = bytearray(chunk_size)
    gen1.fill_chunk(buffer1a)
    hash1a = hash_buffer(buffer1a)
    
    gen1.set_seed(None)  # Switch to non-deterministic
    buffer1b = bytearray(chunk_size)
    gen1.fill_chunk(buffer1b)
    hash1b = hash_buffer(buffer1b)
    
    # Second generator: same sequence
    gen2 = dgen_py.Generator(size=size, seed=12345)
    buffer2a = bytearray(chunk_size)
    gen2.fill_chunk(buffer2a)
    hash2a = hash_buffer(buffer2a)
    
    gen2.set_seed(None)  # Switch to non-deterministic
    buffer2b = bytearray(chunk_size)
    gen2.fill_chunk(buffer2b)
    hash2b = hash_buffer(buffer2b)
    
    print(f"Gen1 chunk 1 (seed=12345): {hash1a}")
    print(f"Gen2 chunk 1 (seed=12345): {hash2a}")
    print(f"Deterministic chunks match: {hash1a == hash2a}")
    print()
    print(f"Gen1 chunk 2 (seed=None): {hash1b}")
    print(f"Gen2 chunk 2 (seed=None): {hash2b}")
    print(f"Non-deterministic chunks differ: {hash1b != hash2b}")
    print()
    
    passed = (hash1a == hash2a) and (hash1b != hash2b)
    print(f"{'✅ PASS' if passed else '❌ FAIL'}: Non-deterministic mode works correctly\n")
    return passed

def test_create_striped_pattern():
    """Demonstrate creating reproducible striped data pattern"""
    print("=" * 80)
    print("DEMO: Creating reproducible striped pattern (A-B-A-B)")
    print("=" * 80)
    
    size = 40 * 1024 * 1024  # 40 MB
    chunk_size = 10 * 1024 * 1024  # 10 MB chunks
    
    # Create pattern: A-B-A-B using two alternating seeds
    gen = dgen_py.Generator(size=size, seed=1111)  # Seed A
    hashes = []
    
    # Stripe 1: A
    gen.set_seed(1111)
    buffer = bytearray(chunk_size)
    gen.fill_chunk(buffer)
    hashes.append(("A", hash_buffer(buffer)))
    
    # Stripe 2: B
    gen.set_seed(2222)
    buffer = bytearray(chunk_size)
    gen.fill_chunk(buffer)
    hashes.append(("B", hash_buffer(buffer)))
    
    # Stripe 3: A (should match Stripe 1)
    gen.set_seed(1111)
    buffer = bytearray(chunk_size)
    gen.fill_chunk(buffer)
    hashes.append(("A", hash_buffer(buffer)))
    
    # Stripe 4: B (should match Stripe 2)
    gen.set_seed(2222)
    buffer = bytearray(chunk_size)
    gen.fill_chunk(buffer)
    hashes.append(("B", hash_buffer(buffer)))
    
    print("Stripe pattern created:")
    for i, (seed_name, hash_val) in enumerate(hashes, 1):
        print(f"  Stripe {i} (seed={seed_name}): {hash_val[:16]}...")
    print()
    
    # Verify A stripes match and B stripes match
    a_match = hashes[0][1] == hashes[2][1]
    b_match = hashes[1][1] == hashes[3][1]
    a_b_differ = hashes[0][1] != hashes[1][1]
    
    print(f"Stripe 1 (A) == Stripe 3 (A): {a_match}")
    print(f"Stripe 2 (B) == Stripe 4 (B): {b_match}")
    print(f"Stripe A != Stripe B: {a_b_differ}")
    print()
    
    passed = a_match and b_match and a_b_differ
    print(f"{'✅ PASS' if passed else '❌ FAIL'}: Striped pattern is reproducible\n")
    return passed

if __name__ == "__main__":
    print("\n")
    print("dgen-py set_seed() Method Test Suite")
    print("=" * 80)
    print()
    
    results = []
    results.append(("Seed change produces different data", test_seed_change_produces_different_data()))
    results.append(("Same seed sequence is reproducible", test_same_seed_sequence_reproducible()))
    results.append(("Non-deterministic mode works", test_switch_to_nondeterministic()))
    results.append(("Striped pattern creation", test_create_striped_pattern()))
    
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
