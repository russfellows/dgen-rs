# Publishing dgen-py to PyPI

This document describes how to publish dgen-py to PyPI using automated GitHub Actions workflows.

## Prerequisites

1. **GitHub Repository**: Set up as a private repository
2. **PyPI Account**: You already have one with a token
3. **GitHub Secrets**: Configure your PyPI token

## Setup Instructions

### 1. Create Private GitHub Repository

```bash
# In your dgen-rs directory
git remote add origin git@github.com:YOUR_USERNAME/dgen-rs.git
git branch -M main
git push -u origin main
```

Then on GitHub:
- Go to repository Settings → General
- Under "Danger Zone" → Change repository visibility → Make private

### 2. Configure GitHub Secrets

Go to: `Settings → Secrets and variables → Actions → New repository secret`

Add your PyPI token:
- **Name**: `PYPI_TOKEN`
- **Value**: Your PyPI token (starts with `pypi-`)

### 3. Publishing Workflow

The automated workflow (`.github/workflows/publish-pypi.yml`) builds wheels for:

**Linux**:
- x86_64 (Intel/AMD 64-bit)
- aarch64 (ARM 64-bit, e.g., AWS Graviton)

**Windows**:
- x64 (64-bit)
- x86 (32-bit, legacy support)

**macOS**:
- x86_64 (Intel Macs)
- aarch64 (Apple Silicon M1/M2/M3)

**Source Distribution**:
- sdist for pip to build on other platforms

### 4. How to Publish

#### Option A: Create a GitHub Release (Recommended)

```bash
# Tag your release
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0

# Create release on GitHub
# Go to: Releases → Draft a new release
# - Choose your tag (v0.1.0)
# - Add release notes
# - Click "Publish release"
```

The workflow automatically triggers on release publication.

#### Option B: Manual Trigger

1. Go to: `Actions → Publish to PyPI`
2. Click `Run workflow`
3. Select branch (main)
4. Click `Run workflow`

### 5. Verify Publication

After ~10-15 minutes, check:
- https://pypi.org/project/dgen-py/
- `pip install dgen-py` on a fresh environment

## Workflow Details

### Build Matrix

The workflow creates wheels for:
- **Python versions**: 3.8, 3.9, 3.10, 3.11, 3.12 (via `--find-interpreter`)
- **Platforms**: Linux (x86_64, aarch64), Windows (x64, x86), macOS (x86_64, aarch64)
- **Total**: ~30-40 wheel files per release

### Build Process

1. **Linux**: Uses `manylinux` containers for maximum compatibility
2. **Windows**: Native Windows builds with MSVC
3. **macOS**: Universal wheels for Intel and Apple Silicon
4. **Source**: sdist for platforms not covered by wheels

### Caching

- Uses `sccache` to cache Rust compilation
- Speeds up subsequent builds significantly
- Shared across workflow runs

## Local Testing

Before publishing, test the build process locally:

```bash
# Install maturin
pip install maturin

# Build wheel for your platform
maturin build --release

# Build source distribution
maturin sdist

# Check the dist/ directory
ls -lh dist/
```

## Version Management

Update version in these files before releasing:

1. **Cargo.toml**: `version = "0.1.0"`
2. **pyproject.toml**: `version = "0.1.0"`
3. **python/dgen_py/__init__.py**: `__version__ = "0.1.0"`

Use semantic versioning:
- **Major** (1.0.0): Breaking API changes
- **Minor** (0.1.0): New features, backward compatible
- **Patch** (0.1.1): Bug fixes

## Troubleshooting

### Build Fails on Specific Platform

Check the Actions log for the specific platform. Common issues:
- **Windows**: MSVC not found → Ensure `PyO3/maturin-action@v1` is used
- **macOS**: Code signing → Not needed for PyPI, ignore warnings
- **Linux**: GLIBC version → manylinux containers handle this

### Upload Fails

- **Duplicate version**: Use `--skip-existing` (already in workflow)
- **Invalid token**: Check `PYPI_TOKEN` secret is correct
- **File too large**: PyPI has 100 MB limit per file

### Missing Platform Support

If you need additional platforms:
- **musllinux**: Add to `linux` job's matrix
- **PyPy**: Add `pypy3.8`, `pypy3.9` to Python versions
- **Other architectures**: Add to matrix (e.g., `armv7`, `s390x`)

## CI/CD Pipeline

The repository has two workflows:

### 1. CI (`.github/workflows/ci.yml`)

Runs on every push and PR:
- Tests on Linux, macOS, Windows
- Tests Python 3.8-3.12
- Runs Rust tests and Python integration tests
- Lints with rustfmt and clippy

### 2. Publish to PyPI (`.github/workflows/publish-pypi.yml`)

Runs on:
- GitHub releases (automatic)
- Manual trigger (workflow_dispatch)

## Best Practices

1. **Always test locally first**: `maturin develop --release`
2. **Update all version numbers**: Cargo.toml, pyproject.toml, __init__.py
3. **Write release notes**: Document changes in the GitHub release
4. **Tag releases**: Use semantic versioning (v0.1.0)
5. **Test after publishing**: `pip install dgen-py` in fresh environment

## Example Release Process

```bash
# 1. Update version numbers
vim Cargo.toml  # version = "0.1.0"
vim pyproject.toml  # version = "0.1.0"
vim python/dgen_py/__init__.py  # __version__ = "0.1.0"

# 2. Update documentation
vim docs/CHANGELOG.md  # Add release notes
vim README.md  # Update version badge

# 3. Commit and tag
git add -A
git commit -m "Release v0.1.0"
git tag -a v0.1.0 -m "Release v0.1.0 - Initial PyPI release"

# 4. Push to GitHub
git push origin main
git push origin v0.1.0

# 5. Create GitHub release
# - Go to GitHub → Releases → Draft new release
# - Choose tag v0.1.0
# - Add release notes from CHANGELOG.md
# - Publish release

# 6. Monitor workflow
# - Go to Actions tab
# - Watch "Publish to PyPI" workflow
# - Check for any failures

# 7. Verify publication
# Wait 10-15 minutes, then:
pip install dgen-py --upgrade
python -c "import dgen_py; print(dgen_py.__version__)"
```

## Security Notes

- **Private repository**: Code is not publicly visible
- **PyPI token**: Stored as GitHub secret, never exposed in logs
- **Trusted publishing**: Can enable PyPI's trusted publisher feature for GitHub Actions (more secure than tokens)

## Support

For issues with:
- **GitHub Actions**: Check workflow logs in Actions tab
- **PyPI upload**: Check https://pypi.org/help/
- **Maturin**: See https://github.com/PyO3/maturin
