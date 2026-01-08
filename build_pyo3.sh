#!/bin/bash
# Build script for Python bindings

set -e  # Exit on error

echo "Building dgen-py Python package..."

# Check if maturin is installed
if ! command -v maturin &> /dev/null; then
    echo "Error: maturin not found. Install with: pip install maturin"
    exit 1
fi

# Build in release mode
echo "Building with maturin..."
maturin develop --release --features python-bindings

echo "Build complete!"
echo ""
echo "Test with:"
echo "  python -c 'import dgen_py; print(dgen_py.generate_data(1024))'"
