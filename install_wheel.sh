#!/bin/bash
# Install dgen-py wheel locally

set -e

echo "Building and installing dgen-py wheel..."

# Build wheel
maturin build --release --features python-bindings

# Find the built wheel
WHEEL=$(ls -t target/wheels/*.whl | head -1)

if [ -z "$WHEEL" ]; then
    echo "Error: No wheel found in target/wheels/"
    exit 1
fi

echo "Installing wheel: $WHEEL"
pip install --force-reinstall "$WHEEL"

echo "Installation complete!"
echo ""
echo "Test with:"
echo "  python -c 'import dgen_py; print(dgen_py.get_system_info())'"
