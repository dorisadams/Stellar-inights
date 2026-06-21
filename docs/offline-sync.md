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
