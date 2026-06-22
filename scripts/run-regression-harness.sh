#!/usr/bin/env bash
# Multi-layer regression harness — runs the representative Stellar RPC /
# contract submission scenario across backend, frontend, and mobile, all
# driven from the shared fixtures in fixtures/contract-flow.json.
# Usage: bash scripts/run-regression-harness.sh
# Exit 0 = all layers pass, exit 1 = one or more regressed.
#
# See docs/regression-harness.md for what each layer covers and how to run
# an individual layer in isolation while iterating.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
PASS=0
FAIL=0
RESULTS=()

run_check() {
  local label="$1"
  shift

  echo ""
  echo "══════════════════════════════════════════"
  echo "  Running: $label"
  echo "══════════════════════════════════════════"

  if (cd "$REPO_ROOT" && "$@"); then
    PASS=$((PASS+1))
    RESULTS+=("[PASS] $label")
  else
    FAIL=$((FAIL+1))
    RESULTS+=("[FAIL] $label")
  fi
}

run_check "backend: Soroban RPC integration tests" \
  bash -c "cd backend && cargo test --test stellar_rpc_integration_test"

run_check "frontend: contract submission flow tests" \
  bash -c "cd frontend && npx vitest run src/__tests__/contractSubmission.test.ts"

run_check "mobile: offline contract submission retry tests" \
  bash -c "cd mobile && npx jest src/hooks/__tests__/useOfflineQueue.contractFlow.test.ts"

TOTAL=$((PASS+FAIL))

echo ""
echo "══════════════════════════════════════════"
echo "  REGRESSION HARNESS SUMMARY"
echo "══════════════════════════════════════════"
for r in "${RESULTS[@]}"; do
  echo "  $r"
done
echo ""
echo "[SUMMARY] $PASS/$TOTAL layers passed"
echo "══════════════════════════════════════════"

if [[ $FAIL -eq 0 ]]; then
  exit 0
else
  exit 1
fi
