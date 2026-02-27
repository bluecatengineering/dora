# Dora DHCP Server -- Development & CI Task Runner
#
# Run `just` or `just --list` to see all available targets.

# Default: show available recipes
default:
    @just --list --unsorted

# ─── Rust ────────────────────────────────────────────────────────────────

# Format all Rust code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run cargo check (fast compilation check)
check:
    SQLX_OFFLINE=true cargo check --all-features

# Run clippy lints
clippy:
    SQLX_OFFLINE=true cargo clippy --all-features -- -D warnings

# Run the full Rust test suite
test:
    SQLX_OFFLINE=true cargo test --all-features --exclude register_derive_impl --workspace

# Run tests for a specific crate
test-crate crate:
    SQLX_OFFLINE=true cargo test --all-features -p {{ crate }}

# Run the dhcp-loadtest smoke test
test-loadtest:
    SQLX_OFFLINE=true cargo test -p dhcp-loadtest

# Build all workspace crates in release mode
build:
    SQLX_OFFLINE=true cargo build --release

# Build the dora binary only
build-dora:
    SQLX_OFFLINE=true cargo build --release -p dora

# Build the dhcp-loadtest tool only
build-loadtest:
    SQLX_OFFLINE=true cargo build --release -p dhcp-loadtest

# Clean build artifacts
clean:
    cargo clean

# Run all pre-commit checks (fmt, clippy, test)
pre-commit: fmt-check clippy test

# ─── Nix Packages ───────────────────────────────────────────────────────

# Build the dora Nix package
nix-build-dora:
    nix build .#default -L

# Build the dhcp-loadtest Nix package
nix-build-loadtest:
    nix build .#dhcp-loadtest -L

# Build all Nix packages
nix-build-all: nix-build-dora nix-build-loadtest

# Enter the Nix development shell
nix-shell:
    nix develop

# ─── NixOS VM Integration Tests ─────────────────────────────────────────

# Run the NATS cluster integration test (existing)
test-nats:
    nix build .#checks.x86_64-linux.dhcp-nats-jetstream-load --rebuild -L

# Run the standalone client compatibility matrix
test-matrix-standalone:
    nix build .#checks.x86_64-linux.dhcp-client-matrix-standalone --rebuild -L

# Run the NATS client compatibility matrix
test-matrix-nats:
    nix build .#checks.x86_64-linux.dhcp-client-matrix-nats --rebuild -L

# Run both client compatibility matrices
test-matrix: test-matrix-standalone test-matrix-nats

# Build the combined matrix report (runs both matrix tests)
test-matrix-report:
    nix build .#checks.x86_64-linux.dhcp-matrix-report --rebuild -L

# Run ALL NixOS VM integration tests
test-vm: test-nats test-matrix

# ─── Matrix Results ──────────────────────────────────────────────────────

# Show the matrix report (build if needed)
matrix-show:
    @nix build .#checks.x86_64-linux.dhcp-matrix-report --rebuild -L 2>&1
    @echo ""
    @cat result/summary.txt

# Show the matrix as a Markdown table
matrix-md:
    @nix build .#checks.x86_64-linux.dhcp-matrix-report --rebuild -L 2>&1
    @cat result/matrix.md

# Export matrix results as JSON
matrix-json:
    @nix build .#checks.x86_64-linux.dhcp-matrix-report --rebuild -L 2>&1
    @cat result/matrix.json

# Save the current matrix results as a baseline artifact
matrix-save tag="":
    #!/usr/bin/env bash
    set -euo pipefail
    nix build .#checks.x86_64-linux.dhcp-matrix-report --rebuild -L
    dir="artifacts/matrix"
    mkdir -p "$dir"
    commit=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    ts=$(date -u +%Y%m%d-%H%M%S)
    suffix="{{ tag }}"
    name="${ts}-${commit}${suffix:+-$suffix}"
    cp result/matrix.json   "$dir/${name}.json"
    cp result/matrix.md     "$dir/${name}.md"
    cp result/matrix.txt    "$dir/${name}.txt"
    echo "Saved to $dir/${name}.{json,md,txt}"

# Compare current matrix against a baseline file
matrix-diff baseline:
    #!/usr/bin/env bash
    set -euo pipefail
    nix build .#checks.x86_64-linux.dhcp-matrix-report --rebuild -L
    python3 nix/format-matrix-results.py \
        --standalone result/standalone-results.json \
        --nats result/nats-results.json \
        --baseline "{{ baseline }}"

# Compare against the most recent saved baseline
matrix-diff-latest:
    #!/usr/bin/env bash
    set -euo pipefail
    latest=$(ls -t artifacts/matrix/*.json 2>/dev/null | head -1)
    if [ -z "$latest" ]; then
        echo "No baseline found in artifacts/matrix/. Run 'just matrix-save' first."
        exit 1
    fi
    echo "Comparing against: $latest"
    just matrix-diff "$latest"

# ─── Format Results Standalone ───────────────────────────────────────────

# Format standalone results only (no NATS)
matrix-standalone-show:
    @nix build .#checks.x86_64-linux.dhcp-client-matrix-standalone --rebuild -L 2>&1
    @python3 nix/format-matrix-results.py --standalone result/results.json

# Format NATS results only
matrix-nats-show:
    @nix build .#checks.x86_64-linux.dhcp-client-matrix-nats --rebuild -L 2>&1
    @python3 nix/format-matrix-results.py --nats result/results.json

# ─── Development Shortcuts ───────────────────────────────────────────────

# Run dora locally with the example config
run config="example.yaml" db="./dev-leases.db":
    SQLX_OFFLINE=true cargo run --release -- -c {{ config }} -d {{ db }}

# Run dora with debug logging
run-debug config="example.yaml" db="./dev-leases.db":
    DORA_LOG=debug SQLX_OFFLINE=true cargo run --release -- -c {{ config }} -d {{ db }}

# Run the dhcp-loadtest tool with custom args
loadtest *args:
    SQLX_OFFLINE=true cargo run --release -p dhcp-loadtest -- {{ args }}

# Watch for changes and re-check (requires cargo-watch)
watch:
    SQLX_OFFLINE=true cargo watch -x 'check --all-features'

# Generate code coverage report
coverage:
    SQLX_OFFLINE=true cargo llvm-cov --all-features --exclude register_derive_impl --workspace --no-fail-fast --lcov --output-path lcov.info
    @echo "Coverage written to lcov.info"

# ─── CI / Full Pipeline ─────────────────────────────────────────────────

# Run the full CI pipeline locally (Rust checks + NixOS VM tests)
ci: pre-commit test-vm
    @echo "Full CI pipeline passed."

# Run only the Rust CI checks (no VM tests)
ci-rust: fmt-check clippy test
    @echo "Rust CI checks passed."

# Run only the NixOS checks
ci-nix: nix-build-all test-vm test-matrix-report
    @echo "Nix CI checks passed."

# Nix flake check (evaluates all checks, builds all)
flake-check:
    nix flake check -L

# ─── Nix Formatting ─────────────────────────────────────────────────────

# Format all Nix files (requires nixfmt)
nix-fmt:
    find . -name '*.nix' -not -path './.git/*' | xargs nixfmt

# Check Nix formatting without modifying
nix-fmt-check:
    find . -name '*.nix' -not -path './.git/*' | xargs nixfmt --check

# Format everything (Rust + Nix)
fmt-all: fmt nix-fmt

# ─── Info / Help ─────────────────────────────────────────────────────────

# Show workspace crate listing
crates:
    @cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | "\(.name)\t\(.version)\t\(.manifest_path)"' | column -t -s $'\t'

# Show the flake outputs
flake-show:
    nix flake show

# Show available NixOS checks
checks:
    @nix eval --json .#checks.x86_64-linux --apply 'x: builtins.attrNames x' 2>/dev/null | jq -r '.[]'

# Print the generated Python test script for a matrix test (for debugging)
debug-test-script mode="standalone":
    @nix eval --raw ".#checks.x86_64-linux.dhcp-client-matrix-{{ mode }}.driver.testScript" 2>/dev/null
