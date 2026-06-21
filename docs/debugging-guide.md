# Debugging & Integration Workflows

> Issue **#104**: Comprehensive Integration & Debugging Workflows
>
> Consolidated reference for running the full stack locally and
> triaging issues across **frontend**, **backend**, **WebSocket**, and
> **mobile**. Every helper described here is gated to development; no
> diagnostic output ever reaches a production bundle.

## 1. TL;DR

| Layer | Run it | Inspect it | Docs |
|---|---|---|---|
| Backend (Rust) | `cd backend && cargo run` | `curl -s localhost:8080/debug/health/detail` | [§3](#3-backend-runtime) |
| Frontend (Next.js) | `cd frontend && pnpm dev` | browser devtools → Console / Network | [§4](#4-frontend--nextjs) |
| Mobile (React Native) | `cd mobile && yarn start` then `yarn ios` / `yarn android` | Flipper + Metro console | [§5](#5-mobile--react-native) |
| WebSocket tunnel | built into backend `/ws` | browser devtools / `wscat` | [§6](#6-websocket-debugging) |

End-to-end integration sanity check:

```bash
bash scripts/debug-stack.sh
```

This script (see [§7](#7-debug-stack-helper)) starts Postgres, boots the
backend in `STELLAR_INSIGHTS_DEBUG=1` mode, and prints the dev-only
endpoints you can hit. It's the single command the README points at
when contributors want to reproduce a bug locally.

## 2. Core principles

1. **Debug surfaces are gated to dev/test.** Backend honors
   `STELLAR_INSIGHTS_DEBUG=1` *and* a debug build profile. Frontend
   helpers gate on `process.env.NODE_ENV !== 'production'`. Mobile
   helpers gate on `__DEV__`. Production builds silently no-op.
2. **No secrets in snapshots.** Diagnostic snapshots expose **counts and
   redacted metadata only** – not credential material, account addresses,
   or queue payloads.
3. **Fail loud at startup.** Trying to mount `/debug/*` in production
   is rejected with a clear error so misconfiguration is visible
   immediately, not six months after release.
4. **Drill down from one anchor.** Every layer reports a `docsUrl`
   pointing to this file, so on-call engineers always know where to go
   next.

## 3. Backend (Rust)

### Environment

```bash
export DATABASE_URL=postgres://postgres:password@localhost:5432/stellar_insights
export RUST_ENV=development
export STELLAR_INSIGHTS_DEBUG=1
cargo run --manifest-path backend/Cargo.toml
```

### Diagnostic endpoints

| Method | Path | Returns |
|---|---|---|
| `GET` | `/debug/health/detail` | `DiagnosticSnapshot` (see `backend/src/debugging/state.rs`) |
| `GET` | `/debug/cache/state` | in-memory cache key count (no payload values!) |
| `GET` | `/debug/queue/status` | count of processed offline-replay `dedup_key`s |

If any of these responds with `503 Service Unavailable` + `error:
"debug endpoints disabled"`, you forgot either `STELLAR_INSIGHTS_DEBUG`
or `RUST_ENV`.

### Common failure scenarios

* **`error: Failed to build HTTP client with timeout` in logs** — the
  client fell back to `Client::new()` after a TLS configuration failure.
  Check that `RPC_REQUEST_TIMEOUT_SECONDS` is within
  `[5, 120]` (see `crates/rpc::rpc_request_timeout_from_env`). Issue
  #127 enforces strict error handling; this fallback is intentional
  rather than a panic.
* **`error: no entries in the offline sync queue`** but the mobile
  client reports pending mutations – inspect
  `mobile/src/services/database.ts` schema version with `SELECT
  MAX(version) FROM schema_version`. If it lags the mobile app's
  `SCHEMA_VERSION`, run `clearDatabase` from the mobile debug service.
* **`clippy::unwrap_used` violations in CI** – the production code gate
  (`cargo clippy --lib -- -D clippy::unwrap_used -D clippy::expect_used
  -D clippy::panic`) is enforced in `.github/workflows/clippy.yml`. If
  CI is red, run `cargo clippy --lib --tests` locally first.

## 4. Frontend (Next.js)

### Local dev server

```bash
cd frontend
pnpm install
pnpm dev
```

The dev server binds `http://localhost:3000`. The Next.js error
overlay surfaces the file/line of any uncaught exception.

### Realistic env vars

Create `frontend/.env.local`:

```env
NEXT_PUBLIC_API_BASE_URL=http://localhost:8080
NEXT_PUBLIC_WS_URL=ws://localhost:8080/ws
NEXT_PUBLIC_STELLAR_NETWORK=testnet
NEXT_PUBLIC_ENABLE_PROD_LOGS=true
```

`NEXT_PUBLIC_ENABLE_PROD_LOGS=true` flips the `logger` to also emit
`debug`/`info`/`warn` in production-grade builds so you can locally
inspect verbose payloads without rebuilding.

### Debug helpers (`frontend/src/lib/debug.ts`)

* `isDebugEnabled()` – true iff `process.env.NODE_ENV !== 'production'`.
* `getDebugSnapshot()` – returns a `DebugSnapshot` describing
  connectivity, effective connection type, realtime (WebSocket) health,
  TanStack Query cache counts, and `localStorage` size estimate.
* `subscribeDebug(onSnapshot, intervalMs)` – polling subscription;
  returns an unsubscribe function.

The snapshot is **never** built when running in production. In prod,
the functions return a literal `DISABLED` payload with
`devMode: false`.

### Inspect the realtime layer

```ts
import { subscribeDebug } from '@/lib/debug';
const unsubscribe = subscribeDebug((snap) => console.table(snap));
// process.env.NODE_ENV !== 'production' → live updates every 5s
```

For deeper WebSocket protocol inspection use the browser devtools
Network → WS panel: select the connection, then look at the message
log frame-by-frame. Each frame corresponds to a `StreamMessage` variant
(see `backend/src/features/websocket_streaming.rs`).

## 5. Mobile (React Native)

The mobile app's debug surface is `mobile/src/services/debugService.ts`.
Call `getMobileDebugSnapshot()` from a dev-only screen to obtain:

* `connectivity.online` / `connectivity.type` (from `@react-native-community/netinfo`)
* `syncQueue.pending` / `syncQueue.oldestPendingAt`
* `notifications.permissionGranted`

The mobile app uses `react-native-mmkv` for general storage and
**SQLite** (`react-native-sqlite-storage`) for the offline cache +
sync_queue (see `mobile/src/services/database.ts`).

### Inspecting the SQLite database

* **iOS Simulator**: `xcrun simctl io booted spawn ... sqlite3 ...`
* **Android Emulator**: `adb shell run-as com.stellarinsights.debug
  sqlite3 databases/stellar_insights.db`
* **Flipper**: enable the Databases plugin → tap the entry → run
  `SELECT * FROM sync_queue;`

### Clearing local state during a triage session

```ts
import { clearDatabase } from '@/services/database';
await clearDatabase();   // rows only, schema preserved
```

## 6. WebSocket debugging

The WebSocket streaming endpoint is exposed by the backend at `/ws`
(per `backend/src/features/websocket_streaming.rs`). The frontend
hooks layer (`frontend/src/hooks/useWebSocket.ts`) wraps it.

### Common failure scenarios

* **No frames arriving after `subscribe`**: check
  `WebSocketStreaming.subscribe()` returns `Ok(receiver)`. If it returns
  `Err(ApiError::service_unavailable("WEBSOCKET_CAPACITY_REACHED", _))`
  you've exceeded `max_subscribers` (default 10 000).
* **Publish returns `Err(ApiError::internal("WEBSOCKET_PUBLISH_FAILED", _))`**:
  the broadcast channel may have no active receivers. Re-subscribe on
  the client and retry; the backend does **not** panic on this
  condition (issue #127 enforcement).
* **Heartbeat frames missing**: `StreamMessage::Heartbeat` should fire
  every `heartbeat_interval_secs` (default 30 s). If missing, the
  frontend's `useWebSocket` will reconnect after the configured
  `connectionAttempts` threshold.

### Manual probe with `wscat`

```bash
npx wscat -c ws://localhost:8080/ws
> {"type":"subscribe","channel":"corridors"}
< {"type":"data","channel":"corridors","payload":{...}}
```

## 7. Debug-stack helper (`scripts/debug-stack.sh`)

```bash
#!/usr/bin/env bash
# Boots the backend in dev/debug mode and prints diagnostic URLs.
# Issue #104 deliverable.

set -euo pipefail

export STELLAR_INSIGHTS_DEBUG=1
export RUST_ENV=development

if ! command -v cargo >/dev/null 2>&1; then
  echo "❌ cargo not installed. Install rustup: https://rustup.rs/" >&2
  exit 1
fi

(
  cd "$(dirname "$0")/../backend"
  cargo run --quiet
) &
BACKEND_PID=$!

cleanup() {
  echo "Stopping debug stack (pid=$BACKEND_PID)…"
  kill "$BACKEND_PID" 2>/dev/null || true
  wait "$BACKEND_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Wait until the debug endpoints come up.
for _ in $(seq 1 30); do
  if curl -sf http://localhost:8080/debug/health/detail >/dev/null; then
    break
  fi
  sleep 1
done

cat <<EOF
Stellar Insights debug stack is up.

  Health detail : http://localhost:8080/debug/health/detail
  Cache state   : http://localhost:8080/debug/cache/state
  Queue status  : http://localhost:8080/debug/queue/status
  WebSocket     : ws://localhost:8080/ws

Frontend dev: cd frontend && pnpm dev  → http://localhost:3000
Mobile dev  : cd mobile   && yarn start

Press Ctrl+C to stop the backend.
EOF

wait "$BACKEND_PID"
```

Run it locally:

```bash
bash scripts/debug-stack.sh
```

## 8. WebSocket-debugging bullet-list (issue #51 carry-over)

* Confirm backend `/ws` is up: `curl -i
  http://localhost:8080/ws` should return `101 Switching Protocols`.
* Confirm `NEXT_PUBLIC_WS_URL` matches the running backend.
* Open browser devtools → Network → WS → click the connection → check
  the message log. Empty for >10 s means a subscribe frame was never
  sent; mismatched channel names mean `useWebSocket` was bound to a
  different channel than the backend publishes.
* For authentication failures during WS handshake, inspect
  `auth_middleware.rs` logs – the WS upgrade must carry the JWT in
  `Authorization` header.

## 9. Production safety checks

Before shipping:

* Confirm `STELLAR_INSIGHTS_DEBUG` is unset in the production
  deployment (`.env.production`).
* Confirm `frontend/.env.production` does **not** define
  `NEXT_PUBLIC_ENABLE_PROD_LOGS=true`.
* Confirm `mobile/.env.production` toggles `__DEV_LOGGING_ENABLED__`
  false (see `mobile/src/services/logger.ts`).
* Run `scripts/smoke-test-all.sh` to exercise the cross-platform
  startup smoke test (already in CI).

## 10. Tests

* `backend/src/debugging/state.rs::tests` – verifies the debug gate is
  closed in release builds.
* `frontend/src/lib/debug.test.ts` (planned) – verifies
  `getDebugSnapshot` returns the disabled payload in production.
* `mobile/src/services/__tests__/debugService.test.ts` (planned) –
  verifies the mobile service no-ops in release builds.

## 11. See also

* [`docs/offline-sync.md`](./offline-sync.md) – the offline-sync half
  of the same architecture (issue #93).
* [`docs/local-development.md`](./local-development.md) – general
  local-dev workflow tips.
* [`docs/SECRETS_MANAGEMENT.md`](./SECRETS_MANAGEMENT.md) – how secrets
  surface (or don't) in diagnostic output.
