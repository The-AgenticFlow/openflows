#!/bin/bash
# AgentFlow Setup Checker
# Verifies all prerequisites are installed and configured

set -e

echo "🔍 AgentFlow Setup Checker"
echo "============================="
echo ""

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Track if all checks pass
ALL_CHECKS_PASSED=true

# Function to print success
success() {
    echo -e "${GREEN}✓${NC} $1"
}

# Function to print error
error() {
    echo -e "${RED}✗${NC} $1"
    ALL_CHECKS_PASSED=false
}

# Function to print warning
warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

echo "1. Checking System Requirements..."
echo "-----------------------------------"

# Check Rust
if command -v rustc &> /dev/null; then
    RUST_VERSION=$(rustc --version | awk '{print $2}')
    success "Rust $RUST_VERSION is installed"
else
    error "Rust is not installed. Install from: https://rustup.rs/"
fi

# Check Node.js
if command -v node &> /dev/null; then
    NODE_VERSION=$(node --version)
    success "Node.js $NODE_VERSION is installed"
else
    error "Node.js is not installed. Install from: https://nodejs.org/"
fi

# Check Claude Code CLI
# First check CLAUDE_PATH from .env, then fallback to system PATH
CLAUDE_PATH=""
if [ -f ".env" ] && grep -q "^CLAUDE_PATH=" .env; then
    CLAUDE_PATH=$(grep "^CLAUDE_PATH=" .env | cut -d'=' -f2-)
fi

if [ -n "$CLAUDE_PATH" ] && [ "$CLAUDE_PATH" != "claude" ]; then
    # CLAUDE_PATH is set to a specific path
    if [ -x "$CLAUDE_PATH" ]; then
        CLAUDE_VERSION=$("$CLAUDE_PATH" --version 2>&1 || echo "unknown")
        success "Claude Code CLI is installed at $CLAUDE_PATH ($CLAUDE_VERSION)"
    else
        error "CLAUDE_PATH is set to '$CLAUDE_PATH' but the file doesn't exist or isn't executable"
        echo "    See docs/setup-claude-cli.md for setup instructions"
    fi
elif command -v claude &> /dev/null; then
    # Claude is in system PATH
    CLAUDE_VERSION=$(claude --version 2>&1 || echo "unknown")
    success "Claude Code CLI is installed in PATH ($CLAUDE_VERSION)"
else
    error "Claude Code CLI is not installed. Install from: https://claude.ai/download"
    echo "    Or set CLAUDE_PATH in .env to the full path of the claude binary"
    echo "    See docs/setup-claude-cli.md for setup instructions"
fi

# Check Git
if command -v git &> /dev/null; then
    GIT_VERSION=$(git --version | awk '{print $3}')
    success "Git $GIT_VERSION is installed"
else
    error "Git is not installed"
fi

# Optional: Check GitHub CLI
if command -v gh &> /dev/null; then
    GH_VERSION=$(gh --version | head -n 1 | awk '{print $3}')
    success "GitHub CLI $GH_VERSION is installed (optional)"
else
    warning "GitHub CLI is not installed (optional, but helpful)"
fi

echo ""
echo "2. Checking Environment Configuration..."
echo "----------------------------------------"

# Check if .env exists
if [ -f ".env" ]; then
    success ".env file exists"
    
    # Check each required variable
    if grep -q "^ANTHROPIC_API_KEY=" .env && ! grep -q "^ANTHROPIC_API_KEY=$" .env && ! grep -q "^ANTHROPIC_API_KEY=sk-ant-xxx" .env; then
        success "ANTHROPIC_API_KEY is set"
    else
        error "ANTHROPIC_API_KEY is missing or not set in .env"
    fi
    
    # Check LLM provider
    if grep -q "^LLM_PROVIDER=" .env; then
        LLM_PROVIDER=$(grep "^LLM_PROVIDER=" .env | cut -d'=' -f2)
        success "LLM_PROVIDER is set to: $LLM_PROVIDER"
        
        # Check if appropriate API key exists for provider
        if [ "$LLM_PROVIDER" = "openai" ]; then
            if grep -q "^OPENAI_API_KEY=" .env && ! grep -q "^OPENAI_API_KEY=$" .env && ! grep -q "^OPENAI_API_KEY=sk-xxx" .env; then
                success "OPENAI_API_KEY is set"
            else
                error "OPENAI_API_KEY is required when LLM_PROVIDER=openai"
            fi
        elif [ "$LLM_PROVIDER" = "gemini" ]; then
            if grep -q "^GEMINI_API_KEY=" .env && ! grep -q "^GEMINI_API_KEY=$" .env && ! grep -q "^GEMINI_API_KEY=your_gemini_api_key_here" .env; then
                success "GEMINI_API_KEY is set"
            else
                error "GEMINI_API_KEY is required when LLM_PROVIDER=gemini"
            fi

            if grep -q "^GEMINI_MODEL=" .env; then
                GEMINI_MODEL=$(grep "^GEMINI_MODEL=" .env | cut -d'=' -f2- | sed 's/^\"//; s/\"$//')
                if [[ "$GEMINI_MODEL" =~ ^gemini-[a-z0-9.-]+$ ]]; then
                    success "GEMINI_MODEL looks valid: $GEMINI_MODEL"
                else
                    error "GEMINI_MODEL must be a Gemini API model code like gemini-2.5-flash or gemini-2.5-flash-lite"
                fi
            else
                warning "GEMINI_MODEL not set; runtime default is gemini-2.5-flash"
            fi
        elif [ "$LLM_PROVIDER" = "anthropic" ]; then
            if grep -q "^ANTHROPIC_API_KEY=" .env && ! grep -q "^ANTHROPIC_API_KEY=$" .env && ! grep -q "^ANTHROPIC_API_KEY=sk-ant-xxx" .env; then
                success "ANTHROPIC_API_KEY is set for NEXUS"
            else
                error "ANTHROPIC_API_KEY is required when LLM_PROVIDER=anthropic"
            fi
        else
            error "LLM_PROVIDER must be one of: openai, gemini, anthropic"
        fi
    else
        warning "LLM_PROVIDER not set (runtime default may apply, but explicit openai/gemini/anthropic is recommended)"
    fi
    
    if grep -q "^GITHUB_PERSONAL_ACCESS_TOKEN=" .env && ! grep -q "^GITHUB_PERSONAL_ACCESS_TOKEN=$" .env && ! grep -q "^GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx" .env; then
        success "GITHUB_PERSONAL_ACCESS_TOKEN is set"
    else
        error "GITHUB_PERSONAL_ACCESS_TOKEN is missing or not set in .env"
    fi
    
    if grep -q "^GITHUB_REPOSITORY=" .env && ! grep -q "^GITHUB_REPOSITORY=$" .env && ! grep -q "^GITHUB_REPOSITORY=owner/repo" .env; then
        REPO=$(grep "^GITHUB_REPOSITORY=" .env | cut -d'=' -f2)
        success "GITHUB_REPOSITORY is set to: $REPO"
    else
        error "GITHUB_REPOSITORY is missing or not set in .env"
    fi
else
    error ".env file does not exist. Copy from .env.example"
fi

echo ""
echo "3. Checking Project Build..."
echo "----------------------------"

# Check if Cargo.toml exists
if [ -f "Cargo.toml" ]; then
    success "Cargo.toml found"
    
    # Try to check cargo build (without actually building)
    if cargo check --bin real_test &> /dev/null; then
        success "Project compiles successfully"
    else
        warning "Project has compilation issues. Run 'cargo build' to see details."
    fi
else
    error "Cargo.toml not found. Are you in the AgentFlow directory?"
fi

echo ""
echo "4. Checking AgentFlow Configuration..."
echo "--------------------------------------"

# Check for agent personas
if [ -f "orchestration/agent/agents/nexus.agent.md" ]; then
    success "NEXUS persona found"
else
    error "orchestration/agent/agents/nexus.agent.md is missing"
fi

if [ -f "orchestration/agent/agents/forge.agent.md" ]; then
    success "FORGE persona found"
else
    error "orchestration/agent/agents/forge.agent.md is missing"
fi

if [ -f "orchestration/agent/registry.json" ]; then
    success "Worker registry found"
    
    # Count workers
    if command -v jq &> /dev/null; then
        WORKER_COUNT=$(jq '.forge.workers | length' orchestration/agent/registry.json)
        success "Registry has $WORKER_COUNT worker slots configured"
    fi
else
    error "orchestration/agent/registry.json is missing"
fi

echo ""
echo "5. Checking Workspace Directory..."
echo "-----------------------------------"

HOME_DIR="${HOME:-$USERPROFILE}"
WORKSPACE_DIR="$HOME_DIR/.agentflow/workspaces"

if [ -d "$WORKSPACE_DIR" ]; then
    success "AgentFlow workspace directory exists: $WORKSPACE_DIR"
    
    # Check for existing workspaces
    WORKSPACE_COUNT=$(find "$WORKSPACE_DIR" -maxdepth 1 -type d | wc -l)
    if [ $WORKSPACE_COUNT -gt 1 ]; then
        success "Found $((WORKSPACE_COUNT - 1)) existing workspace(s)"
    else
        warning "No existing workspaces (will be created on first run)"
    fi
else
    warning "Workspace directory will be created at: $WORKSPACE_DIR"
fi

echo ""
echo "============================="
if [ "$ALL_CHECKS_PASSED" = true ]; then
    echo -e "${GREEN}✓ All checks passed!${NC}"
    echo ""
    echo "You're ready to run AgentFlow:"
    echo "  cargo run --bin real_test"
    exit 0
else
    echo -e "${RED}✗ Some checks failed${NC}"
    echo ""
    echo "Please fix the issues above before running AgentFlow."
    echo "See TUTORIAL.md for detailed setup instructions."
    exit 1
fi
