# Offline Sync & Local Persistence

> Issue **#93**: Production-Grade Offline Sync & Local Persistence

This document is the canonical, end-to-end reference for how Stellar
Insights keeps the mobile app, frontend cache, and backend queue in
agreement when the network is intermittent.

## High-level flow

```
┌──────────┐  ① mutate (offline)   ┌──────────────────┐
│  React   │ ─────────────────────▶│  mobile SQLite   │
│  Native  │                       │  sync_queue table│
│  screens │                       └────────┬─────────┘
└──────────┘                                │ ② reconnect
                                            ▼
                                   ┌──────────────────┐
                                   │  Mobile replay   │
                                   │  loop            │
                                   └────────┬─────────┘
                                            │ ③ POST /queue/replay
                                            ▼
                                   ┌──────────────────┐
                                   │  Backend         │
                                   │  QueueProcessor  │ ④ idempotent
                                   │  (dedup_key)     │
                                   └────────┬─────────┘
                                            │ ⑤ side-effects
                                            ▼
                                   ┌──────────────────┐
                                   │  App domain      │
                                   │  (favourites,    │
                                   │  events, …)      │
                                   └──────────────────┘
```

## 1. Mobile SQLite schema

`mobile/src/services/database.ts` opens (or migrates) the database
`stellar_insights.db` and applies `SCHEMA_VERSION` migrations
idempotently. Tables:

| Table | Purpose |
|---|---|
| `corridors`, `anchors`, `assets` | Cached read-only entities, keyed by their public id. Rows store a JSON payload + ISO `updated_at`. |
| `sync_queue` | Replayable mutations. KEY = `dedup_key` UNIQUE → idempotent enqueue. |
| `schema_version` | Tracks applied migrations. |

Migrations live in the `MIGRATIONS` constant (one map entry per version)
and are run at every startup. New versions must be **append-only**; old
migrations are never edited.

## 2. Sync queue semantics

`enqueueSync(endpoint, method, payload, dedupKey)` writes a row with a
caller-supplied `dedup_key` (e.g. `"fav:anchor:<uuid>"`). Re-submitting
the same logical mutation uses the same `dedup_key` and the
`ON CONFLICT(dedup_key) DO UPDATE` clause **resets** the row to
`pending`,clears any prior error, and zeroes the attempts counter.
This is the mobile-side half of the idempotency contract.

`markSyncFailed(id, error)` implements exponential backoff:

```
delaySec = min(3600, 5 * 6^(attempts - 1))
```

After `MAX_SYNC_ATTEMPTS = 5` attempts the row is marked `failed` and
will be surfaced for manual reconciliation in the mobile Debug Service
(see §6).

## 3. Backend replay processor

`backend/src/queue/replay.rs::QueueProcessor` consumes messages from the
mobile request body and processes each **once** per `dedup_key` within a
process lifetime. Key invariants:

* **Empty `dedup_key`** is a `QueueReplayError::Invalid` (programmer error).
* **Already-processed `dedup_key`** short-circuits with
  `QueueMessageStatus::Processed` but does **not** re-trigger side-effects.
* **Retries** increment a per-key attempts counter; once it exceeds
  `max_retries` the message resolves as `Failed` (caller surfaces a 4xx).

`QueueProcessorHandler` is the trait production code uses to wire
domain-side effects. `LoggingProcessor` is the no-op default used by
tests so the queue logic can be exercised without a database.

## 4. Sync reconciliation contract

> **The contract:**
> Mobile and backend agree that for any retryable action, the
> `dedup_key` is the canonical identifier of the *logical operation*.
> The pair `(mobile client_id, action_fingerprint)` MUST be stable for
> the lifetime of the action so the backend can de-duplicate.

Practical rules:

1. **Generate `dedup_key` on the client**, never on the server.
2. **Keep keys stable across app restarts** – store them in `sync_queue`,
   not in volatile memory.
3. **Include the entity type** so two entities with the same numeric id
   never collide (`fav:anchor:<uuid>`, `fav:asset:<uuid>`).
4. **Treat replays as success** – the backend returns the same response
   shape on duplicate processing. Errors are only returned for genuine
   processing errors.

## 5. Frontend stale-indicators

`frontend/src/hooks/useLocalStorage.ts` exposes
`isStale(key, maxAgeMs)` so screens can render a "stale" badge when the
data is older than the freshness budget. The hook also accepts a
`version` argument and exposes `invalidate(key)` to clear a cache entry
when a related query changes — see the `useLocalStorage.test.ts` tests
for the contract.

## 6. Diagnostics

* **Mobile**: `getMobileDebugSnapshot()` (in `mobile/src/services/debugService.ts`)
  returns `{ syncQueue.pending, syncQueue.oldestPendingAt }` for the
  "Offline Sync" panel that mounts in dev mode.
* **Backend**: `GET /debug/queue/status` returns the number of processed
  `dedup_key`s for this process when debug mode is enabled; otherwise
  `503 Service Unavailable`.
* **Frontend**: `getDebugSnapshot()` exposes `cache.queries` /
  `cache.staleQueries` so the dev overlay can identify cache drift.

## 7. Failure modes

| Symptom | Likely cause | Recovery |
|---|---|---|
| `sync_queue` rows stuck in `failed` | Action can't be replayed (entity deleted on server, schema drift, etc.) | Operator inspects via DebugService and either manually replays via API or drops the row. |
| `dedup_key` collisions | Mobile client bug; fingerprint missed the entity type | Increase prefix uniqueness and bump `schema_version` if a migration is needed. |
| Mobile replay loop drains DB but queue stays >0 | Backend errors on every replay (e.g. 5xx flood) | Increase mobile retry interval; alert via Sentry. |
| Backend `503` on `/debug/queue/status` | Production deployment with debug flag set | unset `STELLAR_INSIGHTS_DEBUG` env var; audit release process. |

## 8. Tests

* `backend/src/queue/replay.rs::tests::replay_is_idempotent` – same
  message three times ⇒ handler called once.
* `backend/src/queue/replay.rs::tests::retry_then_succeed` – transient
  handler failure eventually transitions to `Processed`.
* `mobile/src/services/__tests__/database.test.ts` – exercises
  `initializeDatabase`, `clearDatabase`, and the sync-queue helpers
  against an in-memory SQLite instance via `react-native-sqlite-storage`
  test-mode.

## 9. See also

* [`docs/debugging-guide.md`](./debugging-guide.md) – how to inspect
  sync queue health end-to-end.
* [`backend/src/queue/mod.rs`](../backend/src/queue/mod.rs) – module
  reference.
* [`mobile/src/services/database.ts`](../mobile/src/services/database.ts) –
  schema + helpers reference.
This document describes how Stellar Insights keeps the mobile app, the web
frontend, and the backend consistent when a client is offline and later
reconnects. It covers the local persistence layer, the offline mutation
queue, the cache-staleness model, and the client/server reconciliation
contract, plus known failure modes and what is intentionally left as
follow-up work.

## Goals

- Mobile actions taken while offline are persisted durably and are not lost.
- Once connectivity returns, queued actions are replayed safely — replaying
  the same action twice (e.g. because a retry raced the original request)
  must not apply it twice.
- Data shown while offline is visibly distinguishable from fresh data, and
  callers have an explicit way to invalidate it once reconnected.
- The contract between client and server is small and explicit enough that
  any new client (mobile, web, future CLI) can implement it.

## Architecture overview

```
┌─────────────┐        ┌──────────────────┐        ┌────────────────────┐
│   Mobile     │        │     Frontend     │        │      Backend       │
│             │        │                  │        │                    │
│ database.ts │        │ useLocalStorage  │        │  queue::            │
│  - tables   │        │  - isStale       │        │  OfflineSyncQueue   │
│  - sync_queue│       │  - invalidate()  │        │  - idempotent apply │
│             │        │                  │        │                    │
│ useOfflineCaching.ts │  lib/api-client  │        │  cache/             │
│  - replayPendingSyncActions()           │        │  - invalidation on  │
│  - invalidates read cache on reconnect  │        │    applied mutation │
└─────────────┘        └──────────────────┘        └────────────────────┘
```

## Mobile: local persistence (`mobile/src/services/database.ts`)

`initializeDatabase()` provisions four tables, each backed by the existing
encrypted MMKV store (`storageUtils`) rather than a native SQLite binding —
this keeps the module fully unit-testable in Jest with no native mocking,
while keeping a SQL-shaped table/row API (`getRow`, `getAllRows`,
`upsertRow`, `deleteRow`) that a real SQLite driver (e.g. `expo-sqlite`) can
be swapped in behind later without touching call sites.

| Table       | Purpose                                              |
|-------------|-------------------------------------------------------|
| `corridors` | Cached corridor records for offline reads             |
| `anchors`   | Cached anchor records for offline reads                |
| `assets`    | Cached asset records for offline reads                 |
| `sync_queue`| Durable queue of offline mutations awaiting replay     |

`clearDatabase()` removes all four tables, including any unreplayed queue
entries — call this on logout, not on every app start.

### The `sync_queue` table

Each row is a `SyncQueueRow`:

```ts
{
  id: string;            // idempotency key — see "Reconciliation contract" below
  method: 'POST' | 'PUT' | 'DELETE';
  resource: string;      // e.g. "/corridors/us-mx"
  payload?: unknown;
  status: 'pending' | 'applied' | 'failed';
  clientTimestamp: string;
  retryCount: number;
  lastError?: string;
}
```

`enqueueSyncAction()` writes a new `pending` row. `getPendingSyncActions()`
returns everything not yet applied. `markSyncActionStatus()` records success
or failure (incrementing `retryCount` on failure). `removeSyncAction()` drops
a row once it has been durably applied server-side.

## Mobile: replay on reconnect (`mobile/src/hooks/useOfflineCaching.ts`)

`replayPendingSyncActions()` drains `sync_queue` in enqueue order, replaying
each row against the resource it targets (via the existing `apiClient`
POST/PUT/DELETE methods) and either removing it (success) or marking it
`failed` with the error message (so it stays queued for the next reconnect).

`useOfflineCache()` now subscribes to the app's online/offline transition in
both directions:

- **online → offline**: unchanged — current query data is snapshotted into
  the read cache (`offline-cache:v1`), as before.
- **offline → online** (new): `replayPendingSyncActions()` runs, then the
  read cache and the React Query cache are both invalidated, and
  `apiClient.reconcileState()` is called so the next reads come from the
  server rather than from data that may now be stale.

This means: data read while offline is explicitly cached and bounded (TTL,
size limit, as before); once back online, that cache is dropped rather than
silently served past its usefulness.

## Frontend: cache staleness (`frontend/src/hooks/useLocalStorage.ts`)

`useLocalStorage` gained an additive 4th return value (existing 3-tuple call
sites are unaffected — extra array elements are simply ignored if not
destructured):

```ts
const [value, setValue, removeValue, { isStale, lastUpdated, invalidate }] =
  useLocalStorage('corridor-rates', defaultRates, { ttlMs: 5 * 60 * 1000 });
```

- `lastUpdated` is the timestamp (ms) of the last `setValue` call, persisted
  alongside the value under a `${key}:__meta` companion key (so the raw
  stored value's JSON shape is unchanged for existing readers).
- `isStale` is `true` once `ttlMs` has elapsed since `lastUpdated` — or
  immediately, if `ttlMs` is set but the value has never been written via
  this hook (e.g. it predates this feature). Without `ttlMs`, `isStale` is
  always `false`.
- `invalidate()` clears the staleness metadata (not the value itself), so a
  component can keep showing the last-known value while it marks it stale
  and triggers a refetch.

Pair this with `apiReconcile()` (below): when `isStale` flips to `true`,
call `apiReconcile(['corridor', 'anchor'], lastUpdated)` and merge the
response before calling `invalidate()`.

## Frontend: reconciliation helper (`frontend/src/lib/api-client.ts`)

`apiReconcile(dataTypes, lastKnownTimestamp?)` wraps the existing
`POST /api/rpc/reconcile` endpoint (already used by the mobile client's
`apiClient.reconcileState()`) so the frontend has the same recovery path:
ask the server for everything that changed since `lastKnownTimestamp`,
without needing a bespoke endpoint per cache.

## Backend: idempotent replay (`backend/src/queue/mod.rs`)

`OfflineSyncQueue` is the server-side half of the contract: a concurrency-safe,
idempotent ledger of which client-generated action ids have already been
applied.

```rust
let queue = OfflineSyncQueue::new();

let outcome = queue.submit(action, |action| async move {
    // apply the mutation, e.g. dispatch to the corridor/anchor handler
    apply_to_resource(&action).await
}).await?;

match outcome {
    OfflineActionOutcome::Applied => { /* mutation ran */ }
    OfflineActionOutcome::Duplicate => { /* already applied, no-op */ }
}
```

Each `action.id` is reserved atomically before `apply` runs, so two
concurrent submissions of the same id (two devices replaying the same
queued mutation, or a client retry racing the original request) cannot both
apply it — the loser observes `Duplicate` immediately. If `apply` fails, the
reservation is released so the same id can be legitimately retried on the
next reconnect.

`OfflineSyncQueue::metrics()` exposes `received` / `applied` / `duplicates`
/ `failed` counters for observability.

**Today this is a library-level primitive, not yet wired to an HTTP route.**
The mobile/frontend replay paths currently call the existing per-resource
REST endpoints directly (the same way the pre-existing `useOfflineQueue`
hook does). Wiring `OfflineSyncQueue` behind a dedicated
`POST /api/sync/offline-actions` endpoint — so server-side idempotency is
actually enforced for live traffic, not just exercised in tests — is the
natural next step and is intentionally left out of this change to avoid
touching the router in the same pass as the persistence/cache work above.

## Reconciliation contract

The shape shared between a queued mobile action and the backend queue:

| Field             | Mobile (`SyncQueueRow`) | Backend (`OfflineAction`) | Notes                                  |
|-------------------|-------------------------|---------------------------|-----------------------------------------|
| Idempotency key   | `id`                    | `id`                      | Must stay stable across retries.        |
| HTTP method       | `method`                | `method`                  | `POST` \| `PUT` \| `DELETE`.            |
| Target            | `resource`              | `resource`                | Resource path the mutation applies to.  |
| Body              | `payload`               | `payload`                 | Opaque JSON, validated by the handler.  |
| Client clock      | `clientTimestamp`       | `client_timestamp`        | For ordering/debugging, not dedup.      |

Dedup is always by `id`, never by `(method, resource, payload)` — two
distinct user actions can legitimately have the same method/resource/payload
(e.g. two identical retries of a price update), and only the client knows
whether they're the same logical action or not.

## Failure modes

- **Apply fails on the backend** (network blip, validation error): the row
  stays in `sync_queue` as `failed` with `lastError` set, and the server-side
  reservation is released. The next reconnect retries it.
- **App is killed mid-replay**: rows not yet removed remain `pending` (or
  `failed`, if they got that far) in durable storage and are retried on the
  next reconnect — `replayPendingSyncActions()` is idempotent to re-running
  from the top.
- **Two devices replay the same action**: the backend queue's atomic
  reservation guarantees only one application; the other device's replay is
  reported as `Duplicate` and should treat that as success (the desired
  state has already been reached).
- **Stale read cache**: bounded by `ttlMs` (frontend) / cache TTL + size
  limit (mobile); explicitly invalidated on reconnect rather than relying on
  TTL expiry alone, since a long offline period plus a short TTL would
  otherwise serve no data at all until expiry catches up.

## Testing

- Mobile: `mobile/src/services/__tests__/database.test.ts` (table CRUD,
  sync_queue lifecycle) and
  `mobile/src/hooks/__tests__/useOfflineCaching.replay.test.ts` (replay
  dispatch, partial-failure continuation).
- Frontend: `frontend/src/__tests__/hooks/useLocalStorage.test.ts` (staleness
  & invalidation) and `frontend/src/__tests__/api-client.test.ts`
  (`apiReconcile`).
- Backend: `backend/src/queue/mod.rs` unit tests (single/duplicate/concurrent
  submission, failure-then-retry, independent ids).

Run all three with `scripts/verify-offline-sync.sh`.
