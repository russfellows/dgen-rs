#!/bin/bash
# Build script for Python wheel

set -e  # Exit on error

echo "Building dgen-py Python wheel..."

# Check if maturin is installed
if ! command -v maturin &> /dev/null; then
    echo "Error: maturin not found. Install with: pip install maturin"
    exit 1
fi

# Create wheels directory
WHEEL_DIR="./target/wheels"
mkdir -p "$WHEEL_DIR"

# Build wheel in release mode
echo "Building wheel with maturin (release mode)..."
maturin build --release --out "$WHEEL_DIR"

echo ""
echo "✓ Build complete!"
echo ""
echo "Wheel saved to: $WHEEL_DIR"
ls -lh "$WHEEL_DIR"/*.whl 2>/dev/null || echo "No wheels found"
echo ""
echo "To install locally:"
echo "  pip install $WHEEL_DIR/*.whl --force-reinstall"
echo ""
echo "To install in development mode instead:"
echo "  maturin develop --release"
