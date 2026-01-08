#!/bin/bash
#
# Setup script for creating GitHub repository and configuring PyPI publishing
#

set -e

echo "==============================================="
echo "dgen-rs GitHub Repository Setup"
echo "==============================================="
echo ""

# Check if we're in a git repository
if [ ! -d .git ]; then
    echo "ERROR: Not in a git repository. Run this from the dgen-rs directory."
    exit 1
fi

# Get GitHub username
read -p "Enter your GitHub username: " GITHUB_USER

if [ -z "$GITHUB_USER" ]; then
    echo "ERROR: GitHub username is required"
    exit 1
fi

REPO_NAME="dgen-rs"

echo ""
echo "Creating GitHub repository: $GITHUB_USER/$REPO_NAME"
echo ""

# Check if gh CLI is installed
if ! command -v gh &> /dev/null; then
    echo "GitHub CLI (gh) not found. Please install it:"
    echo "  https://cli.github.com/"
    echo ""
    echo "Or create the repository manually:"
    echo "  1. Go to https://github.com/new"
    echo "  2. Repository name: $REPO_NAME"
    echo "  3. Make it private"
    echo "  4. Don't initialize with README (we already have one)"
    echo "  5. Click 'Create repository'"
    echo ""
    echo "Then run:"
    echo "  git remote add origin git@github.com:$GITHUB_USER/$REPO_NAME.git"
    echo "  git branch -M main"
    echo "  git push -u origin main"
    exit 1
fi

# Create repository using gh CLI
echo "Creating private repository using GitHub CLI..."
gh repo create "$REPO_NAME" \
    --private \
    --description "High-performance random data generation with NUMA optimization and zero-copy Python bindings" \
    --source=. \
    --remote=origin \
    --push

echo ""
echo "✓ Repository created and pushed!"
echo ""

# Instructions for PyPI token
echo "==============================================="
echo "Next Steps: Configure PyPI Publishing"
echo "==============================================="
echo ""
echo "1. Add your PyPI token to GitHub Secrets:"
echo "   a. Go to: https://github.com/$GITHUB_USER/$REPO_NAME/settings/secrets/actions"
echo "   b. Click 'New repository secret'"
echo "   c. Name: PYPI_TOKEN"
echo "   d. Value: <paste your PyPI token>"
echo "   e. Click 'Add secret'"
echo ""
echo "2. To publish a release:"
echo "   a. Update version in Cargo.toml, pyproject.toml, __init__.py"
echo "   b. git tag -a v0.1.0 -m 'Release v0.1.0'"
echo "   c. git push origin v0.1.0"
echo "   d. Go to: https://github.com/$GITHUB_USER/$REPO_NAME/releases/new"
echo "   e. Choose tag v0.1.0, add release notes, click 'Publish release'"
echo ""
echo "3. The GitHub Action will automatically build and publish to PyPI!"
echo ""
echo "See docs/PUBLISHING.md for detailed instructions."
echo ""
