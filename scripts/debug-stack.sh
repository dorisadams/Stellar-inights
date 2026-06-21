#!/usr/bin/env bash
# scripts/debug-stack.sh
#
# Issue #104 – Comprehensive Integration & Debugging Workflows
#
# Boots the Stellar Insights backend in development/debug mode and prints
# the dev-only diagnostic endpoints that contributors can hit.
#
# Usage:   bash scripts/debug-stack.sh
# Stops with Ctrl+C (the script forwards SIGINT/SIGTERM to the backend).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

log() {
  printf '[debug-stack] %s\n' "$*"
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf '❌ %s is not installed or not in PATH. See %s\n' \
      "$1" "${REPO_ROOT}/docs/debugging-guide.md" >&2
    exit 1
  fi
}

require_cmd cargo

# Configurable via env vars; sensible defaults mirror docs/debugging-guide.md.
export STELLAR_INSIGHTS_DEBUG="${STELLAR_INSIGHTS_DEBUG:-1}"
export RUST_ENV="${RUST_ENV:-development}"
PORT="${PORT:-8080}"

log "Starting backend in debug mode on port ${PORT}…"
log "  RUST_ENV=${RUST_ENV}"
log "  STELLAR_INSIGHTS_DEBUG=${STELLAR_INSIGHTS_DEBUG}"

(
  cd "${REPO_ROOT}/backend"
  cargo run --quiet
) &
BACKEND_PID=$!

cleanup() {
  log "Stopping debug stack (pid=${BACKEND_PID})…"
  kill "${BACKEND_PID}" 2>/dev/null || true
  wait "${BACKEND_PID}" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Wait until the debug endpoints are reachable.
for i in $(seq 1 30); do
  if curl -sf "http://localhost:${PORT}/debug/health/detail" >/dev/null; then
    break
  fi
  if ! kill -0 "${BACKEND_PID}" 2>/dev/null; then
    log "❌ backend process exited before becoming ready"
    exit 1
  fi
  sleep 1
done

cat <<EOF
========================================================
Stellar Insights debug stack is up (port ${PORT}).

  Health detail : http://localhost:${PORT}/debug/health/detail
  Cache state   : http://localhost:${PORT}/debug/cache/state
  Queue status  : http://localhost:${PORT}/debug/queue/status
  WebSocket     : ws://localhost:${PORT}/ws

Companion commands:
  Frontend dev: cd frontend && pnpm dev  → http://localhost:3000
  Mobile dev  : cd mobile   && yarn start

More: docs/debugging-guide.md

Press Ctrl+C to stop the backend.
========================================================
EOF

wait "${BACKEND_PID}"
