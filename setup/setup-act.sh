#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if running as root
if [ "$EUID" -eq 0 ]; then
    print_error "Please do not run this script as root. It will use sudo when needed."
    exit 1
fi

print_info "Starting GitHub Actions local testing setup..."

# Check if Docker is installed
if ! command -v docker &> /dev/null; then
    print_error "Docker is not installed. Please install Docker first."
    exit 1
fi

print_info "Docker is installed"

# Add user to docker group if not already
if ! groups | grep -q docker; then
    print_info "Adding user to docker group..."
    sudo usermod -aG docker $USER
    print_warn "User added to docker group. You'll need to log out and back in for this to take effect."
    NEED_RELOGIN=true
else
    print_info "User already in docker group"
    NEED_RELOGIN=false
fi

# Install act
print_info "Installing act..."
if command -v act &> /dev/null; then
    print_warn "act is already installed ($(act --version))"
else
    curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/nektos/act/master/install.sh | sudo bash
    sudo mv ./bin/act /usr/local/bin/ 2>/dev/null || true
    print_info "act installed: $(act --version)"
fi

# Get the directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOCKERFILE_PATH="$SCRIPT_DIR/Dockerfile.act"

# Check if Dockerfile exists
if [ ! -f "$DOCKERFILE_PATH" ]; then
    print_error "Dockerfile.act not found at $DOCKERFILE_PATH"
    print_error "Please ensure the Dockerfile.act is in the same directory as this script"
    exit 1
fi

print_info "Using Dockerfile at $DOCKERFILE_PATH"

# Build Docker image
print_info "Building Docker image debian-trixie-node24:act..."
print_warn "This may take a few minutes on first run..."

if $NEED_RELOGIN; then
    # Need to use sudo because user isn't in docker group yet in this session
    sudo docker build -f "$DOCKERFILE_PATH" -t debian-trixie-node24:act "$SCRIPT_DIR"
else
    docker build -f "$DOCKERFILE_PATH" -t debian-trixie-node24:act "$SCRIPT_DIR"
fi

print_info "Docker image built successfully"

# Configure act for current user
print_info "Configuring act for current user..."
mkdir -p ~/.config/act
cat > ~/.config/act/actrc << 'EOF'
-P ubuntu-latest=debian-trixie-node24:act
--pull=false
EOF
print_info "User config created at ~/.config/act/actrc"

# Configure act for root (when using sudo)
print_info "Configuring act for sudo usage..."
sudo mkdir -p /root/.config/act
echo "-P ubuntu-latest=debian-trixie-node24:act" | sudo tee /root/.config/act/actrc > /dev/null
echo "--pull=false" | sudo tee -a /root/.config/act/actrc > /dev/null
print_info "Root config created at /root/.config/act/actrc"

# Test installation
print_info "Testing act installation..."
if $NEED_RELOGIN; then
    if sudo act --list > /dev/null 2>&1; then
        print_info "act is working correctly (using sudo)"
    else
        print_warn "act test failed, but installation completed"
    fi
else
    if act --list > /dev/null 2>&1; then
        print_info "act is working correctly"
    else
        print_warn "act test failed, but installation completed"
    fi
fi

echo ""
print_info "Setup completed successfully!"
echo ""
print_info "Usage:"
if $NEED_RELOGIN; then
    echo "  - For now, use: sudo act -j <job-name>"
    echo "  - After logout/login, use: act -j <job-name>"
else
    echo "  - Run specific job: act -j <job-name>"
    echo "  - List all jobs: act --list"
    echo "  - Run all jobs: act"
fi
echo ""
print_info "Available commands:"
echo "  act --list              # List all workflow jobs"
echo "  act -j test             # Run specific job"
echo "  act                     # Run all jobs"
echo "  act -j test -v          # Run with verbose output"
echo "  act -n                  # Dry run"
echo ""

if $NEED_RELOGIN; then
    print_warn "IMPORTANT: You were added to the docker group."
    print_warn "Please log out and back in to use 'act' without sudo."
fi
