# Building the Debian Package

This document explains how to build the dora-dhcp Debian package.

## Prerequisites

Install the required build dependencies:

```bash
sudo apt install debhelper devscripts build-essential
sudo apt install cargo rustc pkg-config libc6-dev libsqlite3-dev
```

For Rust, ensure you have at least version 1.70:
```bash
rustc --version
```

If you need to update Rust:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Building the Package

### Method 1: Using dpkg-buildpackage (Recommended)

From the project root directory:

```bash
# Clean any previous builds
cargo clean

# Build the package
dpkg-buildpackage -us -uc -b

# The .deb file will be created in the parent directory
ls -lh ../dora-dhcp_*.deb
```

Options:
- `-us`: Do not sign the source package
- `-uc`: Do not sign the changes file
- `-b`: Binary-only build (no source package)

### Method 2: Using debuild

```bash
# Install debuild if not already installed
sudo apt install devscripts

# Build with debuild
debuild -us -uc -b

# Package will be in parent directory
ls -lh ../dora-dhcp_*.deb
```

### Method 3: Manual build with dpkg-buildpackage

```bash
# Clean build directory
fakeroot debian/rules clean

# Build binaries
debian/rules build

# Create package
fakeroot debian/rules binary

# Package in parent directory
ls -lh ../dora-dhcp_*.deb
```

## Installing the Package

After building:

```bash
sudo dpkg -i ../dora-dhcp_*.deb
```

If there are dependency issues:

```bash
sudo apt --fix-broken install
```

## Verifying the Package

Check package contents:

```bash
dpkg -c ../dora-dhcp_*.deb
```

Check package information:

```bash
dpkg -I ../dora-dhcp_*.deb
```

## Package Contents

The package includes:

**Binary:**
- `/usr/bin/dora` - DHCP server

**Documentation:**
- `/usr/share/doc/dora-dhcp/README.md.gz`
- `/usr/share/doc/dora-dhcp/README.Debian`
- `/usr/share/doc/dora-dhcp/examples/example.yaml` - Example configuration
- `/usr/share/doc/dora-dhcp/examples/config_schema.json` - Config schema
- `/usr/share/doc/dora-dhcp/copyright`
- `/usr/share/doc/dora-dhcp/changelog.Debian.gz`

**Data Directories:**
- `/var/lib/dora/` - Database and runtime data
- `/etc/dora/` - Configuration directory

## Troubleshooting Build Issues

### Cargo/Rust Issues

If cargo fails to build:

```bash
# Update Rust
rustup update

# Clean cargo cache
cargo clean
rm -rf ~/.cargo/registry
```

### Missing Dependencies

If build fails due to missing dependencies:

```bash
# Install build dependencies from debian/control
sudo apt build-dep .
```

### Permission Issues

If you get permission errors:

```bash
# Clean with sudo
sudo cargo clean
sudo debian/rules clean

# Fix ownership
sudo chown -R $USER:$USER .

# Try building again
dpkg-buildpackage -us -uc -b
```

### SQLite Issues

If you get errors about SQLite:

```bash
# Make sure libsqlite3-dev is installed
sudo apt install libsqlite3-dev pkg-config

# Verify pkg-config can find sqlite3
pkg-config --libs sqlite3
```

## Cross-Compilation

To build for different architectures:

```bash
# Install cross-compilation tools
sudo apt install crossbuild-essential-arm64

# Add Rust target
rustup target add aarch64-unknown-linux-gnu

# Configure cargo for cross-compilation
# Edit ~/.cargo/config.toml:
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"

# Build for ARM64
dpkg-buildpackage -aarm64 -us -uc -b
```

### Using cross

For easier cross-compilation, use the `cross` tool:

```bash
# Install cross
cargo install cross

# Build for ARM
cross build --target armv7-unknown-linux-gnueabihf --bin dora --release

# Or for ARM64
cross build --target aarch64-unknown-linux-gnu --bin dora --release
```

## Building Source Package

To create a source package for upload to repositories:

```bash
# Build source package
dpkg-buildpackage -S -us -uc

# This creates:
# - dora_*.dsc
# - dora_*.tar.xz
# - dora_*.changes
```

## Signing Packages

For official releases, sign the packages:

```bash
# Build and sign
dpkg-buildpackage -sa

# Or sign after building
debsign ../dora_*.changes
```

## Creating a Repository

To create a local APT repository:

```bash
# Install reprepro
sudo apt install reprepro

# Create repository structure
mkdir -p ~/apt-repo/conf

# Configure repository
cat > ~/apt-repo/conf/distributions <<EOF
Origin: dora-dhcp
Label: dora-dhcp
Codename: stable
Architectures: amd64 arm64 armhf
Components: main
Description: Dora DHCP server
SignWith: your-gpg-key-id
EOF

# Add package to repository
reprepro -b ~/apt-repo includedeb stable ../dora-dhcp_*.deb

# Use repository
# Add to /etc/apt/sources.list:
# deb [trusted=yes] file:///home/user/apt-repo stable main
```

## CI/CD Integration

For automated builds, use GitHub Actions or GitLab CI:

```yaml
# .github/workflows/build-deb.yml
name: Build Debian Package

on: [push, pull_request]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Install dependencies
        run: |
          sudo apt update
          sudo apt install -y debhelper devscripts cargo rustc libsqlite3-dev
      - name: Build package
        run: dpkg-buildpackage -us -uc -b
      - name: Upload artifact
        uses: actions/upload-artifact@v2
        with:
          name: debian-package
          path: ../*.deb
```

## Development Builds

For development, you can build and test without creating a package:

```bash
# Build dora binary
cargo build --release

# The binary will be at:
# target/release/dora

# Run directly:
sudo target/release/dora -c example.yaml -d test.db
```

## Testing the Package

After installation, verify dora works:

```bash
# Check version
dora --version

# View help
dora --help

# Test with example config
sudo cp /usr/share/doc/dora-dhcp/examples/example.yaml /etc/dora/dora.yaml
# Edit /etc/dora/dora.yaml for your network
sudo dora -c /etc/dora/dora.yaml -d /var/lib/dora/dora.db
```

## Support

For build issues, report at:
https://github.com/bluecatengineering/dora/issues
