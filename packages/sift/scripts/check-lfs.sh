#!/bin/sh
# Check that Git LFS is installed and initialized.
# Run before building to ensure WASM binaries are real files, not LFS pointers.

set -e

if ! command -v git-lfs >/dev/null 2>&1; then
  echo "ERROR: git-lfs is not installed."
  echo "Install it: brew install git-lfs  (or see https://git-lfs.com)"
  exit 1
fi

# Check if any LFS files are pointer files (not fetched)
POINTERS=$(git lfs ls-files --include="*.wasm" 2>/dev/null | grep -c "^-" || true)
if [ "$POINTERS" -gt 0 ]; then
  echo "ERROR: WASM files are LFS pointers, not real files."
  echo "Run: git lfs pull"
  exit 1
fi

echo "Git LFS OK"
