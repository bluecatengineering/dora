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
echo "  GitHub Issue Triage Setup with Claude"
echo "============================================"
echo ""

# Check if gh CLI is installed
if ! command -v gh &> /dev/null; then
    print_error "GitHub CLI (gh) is not installed."
    echo ""
    echo "Please install it from: https://cli.github.com/"
    echo ""
    echo "On Debian/Ubuntu:"
    echo "  sudo apt install gh"
    echo ""
    exit 1
fi

print_info "GitHub CLI is installed: $(gh --version | head -1)"

# Check if authenticated
if ! gh auth status &> /dev/null; then
    print_warn "Not authenticated with GitHub CLI"
    echo ""
    echo "Authentication options:"
    echo "  1. Use a Personal Access Token (no browser needed)"
    echo "  2. Use device authentication (requires browser)"
    echo ""
    read -p "Choose option (1/2) [1]: " auth_method
    auth_method=${auth_method:-1}

    echo ""
    if [ "$auth_method" = "1" ]; then
        print_info "Using Personal Access Token authentication"
        echo ""
        echo "Create a token at: https://github.com/settings/tokens/new"
        echo "Required scopes: repo, workflow"
        echo ""
        read -sp "Enter your GitHub Personal Access Token: " GH_TOKEN
        echo ""

        if [ -z "$GH_TOKEN" ]; then
            print_error "No token provided"
            exit 1
        fi

        echo "$GH_TOKEN" | gh auth login --with-token --git-protocol ssh

        if ! gh auth status &> /dev/null; then
            print_error "Authentication failed. Please check your token."
            exit 1
        fi
        print_info "Successfully authenticated with token!"
    else
        print_info "Starting device authentication with SSH..."
        echo ""
        gh auth login --git-protocol ssh
        echo ""
        if ! gh auth status &> /dev/null; then
            print_error "Authentication failed"
            exit 1
        fi
        print_info "Successfully authenticated!"
    fi
    echo ""
fi

print_info "Authenticated with GitHub CLI"
echo ""

# Get current repository
REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo "")
if [ -z "$REPO" ]; then
    print_error "Not in a GitHub repository or repository not found"
    exit 1
fi

print_info "Current repository: $REPO"
echo ""

# Step 1: Create labels
print_step "Creating standard labels for issues and PRs..."
echo ""

declare -A LABELS=(
    ["bug"]="d73a4a"
    ["enhancement"]="a2eeef"
    ["feature"]="0e8a16"
    ["documentation"]="0075ca"
    ["question"]="d876e3"
    ["help-wanted"]="008672"
    ["good-first-issue"]="7057ff"
    ["performance"]="f9d0c4"
    ["security"]="ee0701"
    ["dependencies"]="0366d6"
    ["breaking-change"]="b60205"
    ["needs-investigation"]="fbca04"
    ["needs-tests"]="d4c5f9"
    ["ready-to-merge"]="0e8a16"
    ["priority:critical"]="b60205"
    ["priority:high"]="d93f0b"
    ["priority:medium"]="fbca04"
    ["priority:low"]="0e8a16"
)

for label in "${!LABELS[@]}"; do
    color="${LABELS[$label]}"
    if gh label create "$label" --color "$color" --force 2>/dev/null; then
        print_info "Created/Updated label: $label"
    else
        print_warn "Could not create label: $label (may already exist)"
    fi
done

echo ""
print_info "Labels created successfully"
echo ""

# Step 2: Check for API key
print_step "Checking Anthropic API key..."
echo ""

# Check if secret already exists
if gh secret list 2>/dev/null | grep -q "ANTHROPIC_API_KEY"; then
    print_info "ANTHROPIC_API_KEY is already set in repository secrets"
    echo ""
else
    print_warn "ANTHROPIC_API_KEY not found in repository secrets"
    echo ""
    echo "You need to add your Anthropic API key as a GitHub secret."
    echo ""
    echo "Options:"
    echo "  1. Set it up manually via GitHub web interface"
    echo "  2. Set it up now using GitHub CLI (requires API key)"
    echo "  3. Skip for now (set it later)"
    echo ""
    read -p "Choose an option (1/2/3) [3]: " choice
    choice=${choice:-3}

    case $choice in
        1)
            print_info "Manual setup instructions:"
            echo ""
            echo "1. Go to: https://github.com/$REPO/settings/secrets/actions"
            echo "2. Click 'New repository secret'"
            echo "3. Name: ANTHROPIC_API_KEY"
            echo "4. Value: Your Anthropic API key from https://console.anthropic.com/"
            echo "5. Click 'Add secret'"
            echo ""
            ;;
        2)
            echo ""
            read -sp "Enter your Anthropic API key: " API_KEY
            echo ""
            if [ -n "$API_KEY" ]; then
                if gh secret set ANTHROPIC_API_KEY --body "$API_KEY" 2>/dev/null; then
                    print_info "API key set successfully"
                else
                    print_error "Failed to set API key. Try manual setup instead."
                fi
            else
                print_warn "No API key provided. Skipping."
            fi
            ;;
        3)
            print_warn "Skipped API key setup. You'll need to set it up before the workflow can run."
            echo ""
            echo "To set it up later:"
            echo "  gh secret set ANTHROPIC_API_KEY"
            echo ""
            ;;
        *)
            print_warn "Invalid choice. Skipping API key setup."
            ;;
    esac
fi

echo ""
print_step "Verifying workflow files..."

ISSUE_WORKFLOW=".github/workflows/issue-triage.yml"
PR_WORKFLOW=".github/workflows/pr-review.yml"

if [ -f "$ISSUE_WORKFLOW" ]; then
    print_info "Issue triage workflow exists: $ISSUE_WORKFLOW"
else
    print_warn "Issue triage workflow not found: $ISSUE_WORKFLOW"
fi

if [ -f "$PR_WORKFLOW" ]; then
    print_info "PR review workflow exists: $PR_WORKFLOW"
else
    print_warn "PR review workflow not found: $PR_WORKFLOW"
fi

echo ""
print_info "Setup completed!"
echo ""
print_info "Next steps:"
echo "  1. Commit and push the workflow files if you haven't:"
echo "     git add .github/workflows/issue-triage.yml .github/workflows/pr-review.yml"
echo "     git commit -m 'Add automated issue triage and PR review workflows'"
echo "     git push"
echo ""
echo "  2. Ensure ANTHROPIC_API_KEY is set in repository secrets"
echo ""
echo "  3. Test the workflows:"
echo "     - Create a new issue to test issue triage"
echo "     - Create a new PR to test PR review"
echo ""
print_info "The workflows will automatically:"
echo "  - Analyze new issues and PRs with Claude"
echo "  - Post comments with detailed analysis"
echo "  - Apply relevant labels based on the analysis"
echo "  - Flag security concerns and breaking changes"
echo ""
