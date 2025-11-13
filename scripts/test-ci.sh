#!/bin/bash
# Test CI commands locally without act/docker
set -e

echo "=== Running Check ==="
cargo check --all-features

echo -e "\n=== Running Tests ==="
echo "Note: Integration tests in dora-bin require sudo (network namespaces)"
echo "Running unit tests only (integration tests excluded)..."
cargo test --all-features --exclude register_derive_impl --workspace --lib

echo -e "\n=== Running Rustfmt ==="
cargo fmt --all -- --check

echo -e "\n=== Running Clippy ==="
cargo clippy -- -D warnings

echo -e "\n✓ All CI checks passed!"
echo -e "\nTo run integration tests (requires sudo):"
echo "  sudo -E cargo test --package dora-bin --test basic"
