#!/bin/bash

# Verifies the offline sync & local persistence feature (see docs/offline-sync.md):
# - mobile: database.ts + useOfflineCaching.ts replay tests
# - frontend: useLocalStorage staleness tests + api-client reconcile tests
# - backend: queue::OfflineSyncQueue idempotency tests

set -e

COLOR_RED='\033[0;31m'
COLOR_GREEN='\033[0;32m'
COLOR_YELLOW='\033[0;33m'
COLOR_RESET='\033[0m'

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo -e "${COLOR_YELLOW}Mobile: database + offline replay tests${COLOR_RESET}"
(cd "$ROOT_DIR/mobile" && npx jest \
  src/services/__tests__/database.test.ts \
  src/hooks/__tests__/useOfflineCaching.replay.test.ts \
  src/hooks/__tests__/useOfflineQueue.test.ts)

echo -e "${COLOR_YELLOW}Frontend: localStorage staleness + api-client reconcile tests${COLOR_RESET}"
(cd "$ROOT_DIR/frontend" && npx vitest run \
  src/__tests__/hooks/useLocalStorage.test.ts \
  src/__tests__/api-client.test.ts)

echo -e "${COLOR_YELLOW}Backend: offline sync queue idempotency tests${COLOR_RESET}"
(cd "$ROOT_DIR/backend" && cargo test --lib queue::tests)

echo -e "${COLOR_GREEN}Offline sync verification passed.${COLOR_RESET}"
