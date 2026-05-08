#!/bin/bash
# tests/e2e/smoketest.sh
# AgentFlow smoke test runner — exercises all 5 agents against smoketest-app
#
# Usage:
#   ./tests/e2e/smoketest.sh                    # Run full pipeline
#   ./tests/e2e/smoketest.sh --scenario bug_backend  # Run specific scenario
#   ./tests/e2e/smoketest.sh --dry-run          # Show plan without executing
#
# Prerequisites:
#   - .env configured with GITHUB_REPOSITORY=Christiantyemele/smoketest-app
#   - GITHUB_PERSONAL_ACCESS_TOKEN set
#   - ANTHROPIC_API_KEY or PROXY_URL set
#   - cargo build succeeds

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONFIG="$SCRIPT_DIR/smoketest.json"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
MAGENTA='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m'

log_header() { echo -e "\n${MAGENTA}═══════════════════════════════════════════════════════${NC}\n  $1\n${MAGENTA}═══════════════════════════════════════════════════════${NC}\n"; }
log_ok()   { echo -e "${GREEN}✓${NC} $1"; }
log_fail() { echo -e "${RED}✗${NC} $1"; }
log_info() { echo -e "${BLUE}→${NC} $1"; }

SCENARIO=""
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --scenario) SCENARIO="$2"; shift 2 ;;
        --dry-run)  DRY_RUN=true; shift ;;
        --help|-h)
            echo "Usage: $0 [--scenario NAME] [--dry-run] [--help]"
            echo ""
            echo "Scenarios from smoketest.json:"
            python3 -c "import json; [print(f'  {k}') for k in json.load(open('$CONFIG'))['test_scenarios']]" 2>/dev/null || echo "  (install python3 to list scenarios)"
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

log_header "AGENTFLOW SMOKE TEST"

cd "$REPO_ROOT"

if [ ! -f .env ]; then
    log_fail ".env not found. Copy .env.example and configure it."
    exit 1
fi

source .env 2>/dev/null || true

REPO="${GITHUB_REPOSITORY:-Christiantyemele/smoketest-app}"
log_info "Target repository: $REPO"

if [ -z "${GITHUB_PERSONAL_ACCESS_TOKEN:-}" ]; then
    log_fail "GITHUB_PERSONAL_ACCESS_TOKEN not set"
    exit 1
fi

if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -z "${PROXY_URL:-}" ]; then
    log_fail "ANTHROPIC_API_KEY or PROXY_URL must be set"
    exit 1
fi

log_ok "Environment validated"

if $DRY_RUN; then
    log_header "DRY RUN — showing execution plan"
    log_info "Would run: cargo run --bin real_test"
    log_info "Target repo: $REPO"
    log_info "Registry: orchestration/agent/registry.json (5 agents)"
    log_info "Open issues will be discovered by NEXUS at runtime"
    exit 0
fi

log_header "BUILDING AGENTFLOW"
cargo build --bin real_test 2>&1
log_ok "Build succeeded"

log_header "RUNNING ORCHESTRATION"
export GITHUB_REPOSITORY="$REPO"

cargo run --bin real_test 2>&1 | tee /tmp/agentflow-smoketest.log

EXIT_CODE=${PIPESTATUS[0]}

if [ $EXIT_CODE -eq 0 ]; then
    log_ok "Orchestration completed successfully"
else
    log_fail "Orchestration exited with code $EXIT_CODE"
fi

log_header "POST-RUN VERIFICATION"

ISSUES=$(gh issue list -R "$REPO" --state open --json number,title,labels 2>/dev/null || echo "[]")
PRS=$(gh pr list -R "$REPO" --state all --json number,title,state 2>/dev/null || echo "[]")

OPEN_COUNT=$(echo "$ISSUES" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "?")
PR_COUNT=$(echo "$PRS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "?")
MERGED_COUNT=$(echo "$PRS" | python3 -c "import sys,json; d=json.load(sys.stdin); print(sum(1 for p in d if p.get('state')=='MERGED'))" 2>/dev/null || echo "?")

log_info "Open issues remaining: $OPEN_COUNT"
log_info "PRs created: $PR_COUNT"
log_info "PRs merged: $MERGED_COUNT"

echo ""
echo "═══════════════════════════════════════════════════════"
echo "  SMOKE TEST SUMMARY"
echo "═══════════════════════════════════════════════════════"
echo ""
echo "  Target:     $REPO"
echo "  Exit code:  $EXIT_CODE"
echo "  Issues open: $OPEN_COUNT"
echo "  PRs merged:  $MERGED_COUNT / $PR_COUNT"
echo ""
echo "  Full log: /tmp/agentflow-smoketest.log"
echo ""

exit $EXIT_CODE
