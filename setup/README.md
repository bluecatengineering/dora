# Setup Scripts

This directory contains setup scripts and configuration for development tools and workflows.

## GitHub Actions Local Testing

To set up local GitHub Actions testing with `act`:

```bash
./setup/setup-act.sh
```

For detailed documentation, see [GITHUB_ACTIONS_LOCAL.md](./GITHUB_ACTIONS_LOCAL.md)

## Automated Issue Triage and PR Review with Claude

To set up automated issue triage and pull request review using Claude AI:

```bash
./setup/setup-issue-triage.sh
```

This will:
- Create standard labels for issues and PRs
- Help configure your Anthropic API key
- Enable automated triage for new issues
- Enable automated review for pull requests

For detailed documentation:
- [ISSUE_TRIAGE.md](./ISSUE_TRIAGE.md) - Issue triage workflow
- [PR_REVIEW.md](./PR_REVIEW.md) - Pull request review workflow

## Files

### GitHub Actions Local Testing
- **setup-act.sh** - Automated setup script for `act` (GitHub Actions local runner)
- **Dockerfile.act** - Custom Debian Trixie + Node.js 24 image for act
- **GITHUB_ACTIONS_LOCAL.md** - Complete documentation for local workflow testing

### Automated Issue Triage and PR Review
- **setup-issue-triage.sh** - Setup script for issue triage and PR review workflows
- **ISSUE_TRIAGE.md** - Complete documentation for automated issue triage with Claude
- **PR_REVIEW.md** - Complete documentation for automated PR review with Claude
