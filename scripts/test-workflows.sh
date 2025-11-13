#!/bin/bash
# Test GitHub Actions workflows locally with act
set -e

echo "=== GitHub Actions Workflow Testing ==="
echo ""

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Function to print status
print_status() {
    local status=$1
    local message=$2
    if [ "$status" = "OK" ]; then
        echo -e "${GREEN}✓${NC} $message"
    elif [ "$status" = "WARN" ]; then
        echo -e "${YELLOW}⚠${NC} $message"
    else
        echo -e "${RED}✗${NC} $message"
    fi
}

# Step 1: Validate workflow syntax
echo "Step 1: Validating workflow syntax..."
if sudo act --list > /dev/null 2>&1; then
    print_status "OK" "Workflow syntax is valid"
    sudo act --list
else
    print_status "FAIL" "Workflow syntax validation failed"
    exit 1
fi

echo ""
echo "Step 2: Dry run (test execution plan)..."
if sudo act -n push > /dev/null 2>&1; then
    print_status "OK" "Workflow execution plan is valid"
else
    print_status "FAIL" "Workflow execution plan failed"
    exit 1
fi

echo ""
echo "Step 3: Running fmt workflow..."
if sudo act -j fmt push > /tmp/fmt.log 2>&1; then
    print_status "OK" "fmt workflow passed"
    tail -5 /tmp/fmt.log
else
    print_status "FAIL" "fmt workflow failed"
    tail -20 /tmp/fmt.log
    exit 1
fi

echo ""
echo "Step 4: Running clippy workflow (this may take a few minutes)..."
if sudo act -j clippy push > /tmp/clippy.log 2>&1; then
    print_status "OK" "clippy workflow passed"
    tail -5 /tmp/clippy.log
else
    print_status "FAIL" "clippy workflow failed"
    tail -20 /tmp/clippy.log
    exit 1
fi

echo ""
print_status "OK" "All workflow tests completed successfully!"
echo ""
echo "Available workflows:"
echo "  - fmt:      Code formatting check"
echo "  - clippy:   Rust linter"
echo "  - check:    Cargo check (compile check)"
echo "  - test:     Run test suite"
echo "  - coverage: Generate code coverage"
echo ""
echo "Run individual workflows with: sudo act -j <job-name> push"
