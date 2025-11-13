# GitHub Actions Local Testing

This guide explains how to run GitHub Actions workflows locally using `act`.

## Quick Setup

Run the setup script to install and configure everything automatically:

```bash
./setup/setup-act.sh
```

This script will:
1. Check if Docker is installed (required)
2. Install `act` tool for running GitHub Actions locally
3. Add your user to the docker group
4. Build a custom Debian Trixie Docker image with Node.js 24
5. Configure `act` to use the custom image

## Prerequisites

- Docker must be installed and running
- Internet connection (to download act and build Docker image)
- sudo privileges

## Manual Setup

If you prefer to set up manually, see the `setup/setup-act.sh` script for the exact steps.

## Usage

### List available workflow jobs

```bash
act --list
```

### Run a specific job

```bash
act -j fmt          # Run rustfmt
act -j test         # Run tests
act -j check        # Run cargo check
act -j clippy       # Run clippy
act -j coverage     # Run coverage
```

### Run all jobs

```bash
act
```

### Additional options

```bash
act -j test -v      # Verbose output
act -n              # Dry run (show what would run)
act push            # Simulate push event
```

## Configuration Files

- `~/.config/act/actrc` - User configuration
- `/root/.config/act/actrc` - Root configuration (for sudo usage)
- `setup/Dockerfile.act` - Custom Docker image definition (if you want to modify it)

## Custom Docker Image

The setup creates a Docker image `debian-trixie-node24:act` with:
- Debian Trixie base
- Node.js 24 from NodeSource
- Git and essential build tools

## Troubleshooting

### Permission denied error

If you get "permission denied" when connecting to Docker:
- Run with sudo: `sudo act -j <job>`
- Or log out and back in (if you just ran setup-act.sh)

### Image not found

If you get image not found errors:
- Re-run the setup script: `./setup/setup-act.sh`
- Or rebuild manually: `docker build -f setup/Dockerfile.act -t debian-trixie-node24:act .`

### Workflow fails

- Check that SQLX_OFFLINE=true is set in workflow (already configured)
- Use verbose mode to see details: `act -j <job> -v`
- Check Docker has enough disk space

## Notes

- First run of each job will be slower as it downloads Rust toolchains
- Subsequent runs will be faster due to Docker layer caching
- The custom image uses Debian Trixie as requested
- The `--pull=false` flag prevents act from trying to pull images from Docker Hub

## Workflow Jobs

Your repository has these workflow jobs defined in `.github/workflows/actions.yml`:

1. **check** - Run cargo check on stable and beta Rust
2. **test** - Run test suite (excluding register_derive_impl)
3. **fmt** - Check code formatting with rustfmt
4. **clippy** - Run clippy linter with warnings as errors
5. **coverage** - Generate code coverage reports

All jobs use `SQLX_OFFLINE=true` to avoid requiring a database.
