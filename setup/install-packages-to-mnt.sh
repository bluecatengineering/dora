#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

echo "============================================"
echo "  Install GitHub Packages"
echo "============================================"
echo ""

# Detect mode: standalone or chroot
USE_CHROOT=false
TARGET_ROOT="/"

# Get the script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NETCTL_DIR=""

# Try to find ../netctl directory
if [ -d "$SCRIPT_DIR/../netctl" ]; then
    NETCTL_DIR="$(cd "$SCRIPT_DIR/../netctl" && pwd)"
fi

if [ -n "$NETCTL_DIR" ] && [ -d "$NETCTL_DIR/usr" ] && [ -d "$NETCTL_DIR/etc" ]; then
    # Running from host, will use chroot to ../netctl
    USE_CHROOT=true
    TARGET_ROOT="$NETCTL_DIR"
    print_info "Mode: CHROOT to $TARGET_ROOT"
else
    # Running standalone
    print_info "Mode: STANDALONE (current system)"
fi

echo ""

# Packages to be installed
print_step "Packages to be installed:"
echo ""
echo "  1. gh (GitHub CLI) - via apt"
echo "  2. act (GitHub Actions local runner) - via installer"
echo "  3. actionlint (GitHub Actions workflow syntax checker) - via installer"
echo ""

# Dry run option
if [ "$1" = "--dry-run" ] || [ "$1" = "-n" ]; then
    print_info "DRY RUN MODE - No changes will be made"
    echo ""
    echo "Would execute:"
    if [ "$USE_CHROOT" = true ]; then
        echo "  1. chroot $TARGET_ROOT apt update"
        echo "  2. chroot $TARGET_ROOT apt install -y gh"
        echo "  3. Install act to $TARGET_ROOT/usr/local/bin/"
        echo "  4. Install actionlint to $TARGET_ROOT/usr/local/bin/"
    else
        echo "  1. apt update"
        echo "  2. apt install -y gh"
        echo "  3. Install act to /usr/local/bin/"
        echo "  4. Install actionlint to /usr/local/bin/"
    fi
    echo ""
    print_info "To perform actual installation, run without --dry-run flag"
    exit 0
fi

# Confirm
echo ""
read -p "Proceed with installation? (y/n): " confirm
if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
    print_warn "Installation cancelled"
    exit 0
fi

echo ""

# Define helper function to run commands
run_cmd() {
    if [ "$USE_CHROOT" = true ]; then
        chroot "$TARGET_ROOT" "$@"
    else
        "$@"
    fi
}

# Install gh (GitHub CLI)
print_step "Installing GitHub CLI (gh)..."
echo ""

if run_cmd which gh &>/dev/null; then
    print_info "gh is already installed"
    run_cmd gh --version | head -1
else
    print_info "Updating package lists..."
    run_cmd apt update

    print_info "Installing gh..."
    run_cmd apt install -y gh

    print_info "Verifying installation..."
    run_cmd gh --version
fi

echo ""

# Install act
print_step "Installing act (GitHub Actions runner)..."
echo ""

ACT_PATH="$TARGET_ROOT/usr/local/bin/act"

if [ -f "$ACT_PATH" ]; then
    print_info "act is already installed"
    run_cmd /usr/local/bin/act --version || true
else
    print_info "Downloading and installing act..."

    # Download act installer
    TMP_DIR=$(mktemp -d)
    cd "$TMP_DIR"

    print_info "Downloading act installer..."
    curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/nektos/act/master/install.sh -o install-act.sh

    print_info "Running act installer..."
    bash install-act.sh

    if [ -f "./bin/act" ]; then
        print_info "Installing act to $ACT_PATH"

        # Ensure target directory exists
        mkdir -p "$TARGET_ROOT/usr/local/bin"

        cp ./bin/act "$ACT_PATH"
        chmod +x "$ACT_PATH"

        print_info "Verifying installation..."
        run_cmd /usr/local/bin/act --version
    else
        print_error "act binary not found after installation"
        cd -
        rm -rf "$TMP_DIR"
        exit 1
    fi

    cd -
    rm -rf "$TMP_DIR"
fi

echo ""

# Install actionlint
print_step "Installing actionlint (GitHub Actions workflow syntax checker)..."
echo ""

ACTIONLINT_PATH="$TARGET_ROOT/usr/local/bin/actionlint"

if [ -f "$ACTIONLINT_PATH" ]; then
    print_info "actionlint is already installed"
    run_cmd /usr/local/bin/actionlint --version || true
else
    print_info "Downloading and installing actionlint..."

    # Detect architecture
    ARCH=$(uname -m)
    case $ARCH in
        x86_64)
            ACTIONLINT_ARCH="linux_amd64"
            ;;
        aarch64|arm64)
            ACTIONLINT_ARCH="linux_arm64"
            ;;
        *)
            print_error "Unsupported architecture: $ARCH"
            print_warn "Skipping actionlint installation"
            ACTIONLINT_ARCH=""
            ;;
    esac

    if [ -n "$ACTIONLINT_ARCH" ]; then
        # Download latest actionlint
        TMP_DIR=$(mktemp -d)
        cd "$TMP_DIR"

        print_info "Downloading actionlint for $ACTIONLINT_ARCH..."
        ACTIONLINT_VERSION=$(curl -s https://api.github.com/repos/rhysd/actionlint/releases/latest | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')

        if [ -z "$ACTIONLINT_VERSION" ]; then
            print_warn "Could not determine latest actionlint version, using v1.7.1"
            ACTIONLINT_VERSION="1.7.1"
        fi

        print_info "Installing actionlint v$ACTIONLINT_VERSION..."
        curl -sL "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_${ACTIONLINT_ARCH}.tar.gz" -o actionlint.tar.gz

        tar xzf actionlint.tar.gz

        if [ -f "./actionlint" ]; then
            print_info "Installing actionlint to $ACTIONLINT_PATH"

            # Ensure target directory exists
            mkdir -p "$TARGET_ROOT/usr/local/bin"

            cp ./actionlint "$ACTIONLINT_PATH"
            chmod +x "$ACTIONLINT_PATH"

            print_info "Verifying installation..."
            run_cmd /usr/local/bin/actionlint --version
        else
            print_error "actionlint binary not found after extraction"
        fi

        cd -
        rm -rf "$TMP_DIR"
    fi
fi

echo ""
print_info "Installation completed!"
echo ""
print_info "Installed packages:"
echo ""

# Verify installations
if run_cmd which gh &>/dev/null; then
    echo "  ✓ gh: $(run_cmd gh --version | head -1)"
else
    echo "  ✗ gh: NOT FOUND"
fi

if [ -f "$ACT_PATH" ]; then
    echo "  ✓ act: $(run_cmd /usr/local/bin/act --version)"
else
    echo "  ✗ act: NOT FOUND"
fi

if [ -f "$ACTIONLINT_PATH" ]; then
    echo "  ✓ actionlint: $(run_cmd /usr/local/bin/actionlint --version 2>&1 | head -1)"
else
    echo "  ✗ actionlint: NOT FOUND"
fi

echo ""
print_info "Done!"
echo ""

if [ "$USE_CHROOT" = true ]; then
    print_info "Packages installed to: $TARGET_ROOT"
else
    print_info "Packages installed to current system"
fi

echo ""
print_info "Usage examples:"
echo "  gh auth login                          # Authenticate with GitHub"
echo "  act --list                             # List workflow jobs"
echo "  actionlint .github/workflows/*.yml     # Check workflow syntax"
