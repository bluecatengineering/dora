# GitHub Actions Workflows - Quick Reference

## Local Testing with Docker

This project uses [act](https://github.com/nektos/act) to run GitHub Actions workflows locally in Docker containers.

## Setup

Already configured! If you need to reconfigure:
```bash
./setup/setup-act.sh
```

## Quick Start

### List all workflows
```bash
sudo act --list
```

Output:
```
Stage  Job ID    Job name      Workflow name             Workflow file     Events
0      clippy    Clippy        Actions                   actions.yml       push,pull_request
0      coverage  Run coverage  Actions                   actions.yml       push,pull_request
0      check     Check         Actions                   actions.yml       push,pull_request
0      test      Test Suite    Actions                   actions.yml       push,pull_request
0      fmt       Rustfmt       Actions                   actions.yml       push,pull_request
0      triage    triage        Issue Triage with Claude  issue-triage.yml  issues
0      review    review        PR Review with Claude     pr-review.yml     pull_request
```

### Run workflows

#### Format check (fastest, ~1 minute)
```bash
sudo act -j fmt push
```

#### Linter check (~5 minutes)
```bash
sudo act -j clippy push
```

#### Compile check (~5 minutes, requires more disk space)
```bash
sudo act -j check push
```

#### Run tests
```bash
sudo act -j test push
```

#### Generate coverage
```bash
sudo act -j coverage push
```

### Test workflow syntax only (dry run)
```bash
sudo act -n push
```

## Workflow Details

### Rustfmt (Code Formatting)
- **File**: `.github/workflows/actions.yml`
- **Job**: `fmt`
- **Matrix**: Rust stable
- **Command**: `cargo fmt --all -- --check`
- **Purpose**: Ensures consistent code formatting

### Clippy (Linter)
- **File**: `.github/workflows/actions.yml`
- **Job**: `clippy`
- **Matrix**: Rust stable
- **Command**: `cargo clippy -- -D warnings`
- **Purpose**: Catches common mistakes and improves code quality
- **Note**: All warnings are treated as errors

### Check (Compilation)
- **File**: `.github/workflows/actions.yml`
- **Job**: `check`
- **Matrix**: Rust stable + beta
- **Command**: `cargo check --all-features`
- **Purpose**: Verifies code compiles

### Test Suite
- **File**: `.github/workflows/actions.yml`
- **Job**: `test`
- **Matrix**: Rust stable
- **Command**: `cargo test --all-features --exclude register_derive_impl --workspace`
- **Purpose**: Runs all unit and integration tests

### Coverage
- **File**: `.github/workflows/actions.yml`
- **Job**: `coverage`
- **Command**: `cargo llvm-cov --all-features --workspace --lcov`
- **Purpose**: Generates code coverage reports

## Docker Environment

- **Image**: `debian-trixie-node24:act`
- **Base**: Debian Trixie
- **Includes**:
  - Rust toolchain (installed on-demand)
  - build-essential
  - pkg-config
  - libssl-dev
  - libsqlite3-dev
  - Node.js 24
  - Git

## Environment Variables

All workflows use:
- `SQLX_OFFLINE=true` - Enables offline SQL query verification

## Troubleshooting

### Permission denied
If you get Docker permission errors:
```bash
# Use sudo
sudo act -j fmt push

# Or log out and back in (after running setup-act.sh)
```

### Disk space issues
If workflows fail with "No space left on device":
```bash
# Clean up Docker
sudo docker system prune -a

# Check disk usage
sudo docker system df
```

### Workflow syntax errors
```bash
# Validate syntax
sudo act --list --verbose
```

## Testing Script

Automated testing script available:
```bash
../scripts/test-workflows.sh
```

This script:
1. Validates workflow syntax
2. Runs dry-run test
3. Executes fmt workflow
4. Executes clippy workflow
5. Provides summary report

## CI/CD Integration

These workflows automatically run on GitHub when:
- **push** - All main workflows (check, test, fmt, clippy, coverage)
- **pull_request** - All main workflows
- **issues** - Issue triage workflow
- **pull_request** (opened/reopened) - PR review workflow

## Best Practices

1. **Before committing**: Run `sudo act -j fmt push` and `sudo act -j clippy push`
2. **Before pushing**: Run `sudo act -j test push`
3. **CI failures**: Use local act to reproduce and debug
4. **New features**: Ensure tests pass locally first

## Additional Resources

- [act documentation](https://github.com/nektos/act)
- [GitHub Actions documentation](https://docs.github.com/en/actions)
- [Workflow setup guide](./setup/GITHUB_ACTIONS_LOCAL.md)
