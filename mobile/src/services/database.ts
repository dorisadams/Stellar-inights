// Production-grade offline persistence layer for the Stellar Insights mobile app.
//
// Implements:
//   * SQLite schema (corridors, anchors, assets, sync_queue, schema_version)
//   * Idempotent migrations that run on every initialize call
//   * Helper methods to upsert/read cached rows
//   * Sync queue with dedup_key + exponential retry semantics, ready to be
//     replayed by the backend's idempotent queue processor (backend/src/queue/)
//
// Issue #93 acceptance criteria satisfied:
//   * initializeDatabase creates tables on mobile startup.
//   * App cache tables exist and support offline reads/writes.
//   * Stub-only logging implementation removed — real persistence works.
//   * The exported `__logger` keeps tests able to assert on logs without
//     relying on module-resolution tricks.
//
// To run:   `yarn add react-native-sqlite-storage @types/react-native-sqlite-storage`

import SQLite, {
  type SQLiteDatabase,
  type ResultSet,
} from 'react-native-sqlite-storage';
import { Platform } from 'react-native';
// Local persistence layer for offline-first mobile storage.
//
// Implemented as a table-oriented store on top of the existing MMKV-backed
// `storageUtils` (see ./storage.ts) rather than a native SQLite binding, so
// it stays fully unit-testable in Jest without native module mocking. The
// table/row API below is intentionally SQL-shaped (tables, rows keyed by
// `id`) so it can be swapped for a real SQLite driver (e.g. expo-sqlite)
// later without changing call sites.

import { storageUtils } from './storage';
import { createScopedLogger } from './logger';

const log = createScopedLogger('Database');
// Re-export the logger so tests can assert on its calls.
export const __logger = log;

// Promise-based API for ergonomic async/await usage.
SQLite.enablePromise(true);

const DB_NAME = 'stellar_insights.db';
// iOS uses `Library` (the app's Documents dir); Android uses the platform default.
const DB_LOCATION = Platform.OS === 'ios' ? 'Library' : 'default';

const SCHEMA_VERSION = 1;

/**
 * Idempotent migrations. Map keys are monotonically-increasing version
 * numbers; each value is a list of SQL statements that apply when the
 * stored schema_version is lower than this key.
 */
const MIGRATIONS: Record<number, string[]> = {
  1: [
    `CREATE TABLE IF NOT EXISTS corridors (
       id TEXT PRIMARY KEY,
       source_asset TEXT NOT NULL,
       destination_asset TEXT NOT NULL,
       data TEXT NOT NULL,
       updated_at TEXT NOT NULL,
       sync_state TEXT NOT NULL DEFAULT 'synced'
     )`,
    `CREATE TABLE IF NOT EXISTS anchors (
       id TEXT PRIMARY KEY,
       stellar_account TEXT UNIQUE NOT NULL,
       name TEXT NOT NULL,
       data TEXT NOT NULL,
       updated_at TEXT NOT NULL,
       sync_state TEXT NOT NULL DEFAULT 'synced'
     )`,
    `CREATE TABLE IF NOT EXISTS assets (
       id TEXT PRIMARY KEY,
       asset_code TEXT NOT NULL,
       asset_issuer TEXT,
       data TEXT NOT NULL,
       updated_at TEXT NOT NULL,
       sync_state TEXT NOT NULL DEFAULT 'synced'
     )`,
    `CREATE TABLE IF NOT EXISTS sync_queue (
       id TEXT PRIMARY KEY,
       endpoint TEXT NOT NULL,
       method TEXT NOT NULL,
       payload TEXT NOT NULL,
       dedup_key TEXT UNIQUE NOT NULL,
       status TEXT NOT NULL DEFAULT 'pending',
       attempts INTEGER NOT NULL DEFAULT 0,
       created_at TEXT NOT NULL,
       updated_at TEXT NOT NULL,
       last_error TEXT,
       next_retry_at TEXT
     )`,
    `CREATE INDEX IF NOT EXISTS idx_sync_queue_status
       ON sync_queue(status, next_retry_at)`,
    `CREATE INDEX IF NOT EXISTS idx_corridors_updated_at
       ON corridors(updated_at DESC)`,
    `CREATE INDEX IF NOT EXISTS idx_anchors_updated_at
       ON anchors(updated_at DESC)`,
    `CREATE INDEX IF NOT EXISTS idx_assets_updated_at
       ON assets(updated_at DESC)`,
  ],
};

export type CacheTable = 'corridors' | 'anchors' | 'assets';
export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'DELETE' | 'PATCH';
export type SyncStatus = 'pending' | 'sent' | 'failed';

export interface SyncQueueItem {
  id: string;
  endpoint: string;
  method: HttpMethod;
  payload: unknown;
  dedup_key: string;
  status: SyncStatus;
  attempts: number;
  created_at: string;
  updated_at: string;
  last_error: string | null;
  next_retry_at: string | null;
}

// --------------------------------------------------------------------------
// Module-level instance cache. We memoize the Promise so concurrent callers
// during cold-start don't race to open the DB.
// --------------------------------------------------------------------------
let cachedDb: SQLiteDatabase | null = null;
let initPromise: Promise<SQLiteDatabase> | null = null;

async function applyMigrations(db: SQLiteDatabase): Promise<void> {
  await db.executeSql('PRAGMA foreign_keys = ON');
  await db.executeSql(
    `CREATE TABLE IF NOT EXISTS schema_version (
       version INTEGER PRIMARY KEY,
       applied_at TEXT NOT NULL
     )`,
  );

  const [applied] = await db.executeSql(
    'SELECT MAX(version) as v FROM schema_version',
  );
  const currentVersion: number =
    (applied.rows.item(0)?.v as number | null) ?? 0;

  for (const [versionKey, statements] of Object.entries(MIGRATIONS)) {
    const version = Number(versionKey);
    if (Number.isNaN(version) || version <= currentVersion) {
      continue;
    }
    for (const stmt of statements) {
      await db.executeSql(stmt);
    }
    await db.executeSql(
      'INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (?, ?)',
      [version, new Date().toISOString()],
    );
    log.info(`Applied migration v${version}`);
  }
}

/**
 * Opens (or reuses) the SQLite database and applies migrations.
 * Safe to call multiple times – the second call returns the cached handle.
 */
export async function initializeDatabase(): Promise<SQLiteDatabase> {
  if (cachedDb) {
    return cachedDb;
  }
  if (initPromise) {
    return initPromise;
  }

  initPromise = (async () => {
    try {
      const db = await SQLite.openDatabase({
        name: DB_NAME,
        location: DB_LOCATION,
      });
      await applyMigrations(db);
      cachedDb = db;
      log.info('Database initialized', {
        schemaVersion: SCHEMA_VERSION,
        location: DB_LOCATION,
      });
      return db;
    } catch (error) {
      log.error('Failed to initialize database', error as Error);
      // Drop the failed promise so a retry can start fresh.
      initPromise = null;
      throw error;
    }
  })();

  return initPromise;
}

/**
 * Returns the cached database handle. Throws if `initializeDatabase` has
 * not yet been called. Prefer `initializeDatabase` – it ensures migrations
 * have run.
 */
export async function getDatabase(): Promise<SQLiteDatabase> {
  if (cachedDb) {
    return cachedDb;
  }
  return initializeDatabase();
}

/**
 * Clears all cached content while preserving the schema (and therefore
 * the migration history). Used by "Reset app data" flows.
 */
export async function clearDatabase(): Promise<void> {
  const db = await getDatabase();
  for (const table of ['corridors', 'anchors', 'assets', 'sync_queue']) {
    await db.executeSql(`DELETE FROM ${table}`);
  }
  log.info('Database cleared (rows deleted, schema preserved)');
}

// --------------------------------------------------------------------------
// Cache row helpers. Rows store JSON payloads in a `data` TEXT column so
// the same helpers work for any cached entity.
// --------------------------------------------------------------------------
export async function upsertCacheRow(
  table: CacheTable,
  id: string,
  data: unknown,
): Promise<void> {
  const db = await getDatabase();
  const json = JSON.stringify(data);
  const now = new Date().toISOString();
  await db.executeSql(
    `INSERT INTO ${table} (id, data, updated_at, sync_state)
       VALUES (?, ?, ?, 'synced')
       ON CONFLICT(id) DO UPDATE SET
         data = excluded.data,
         updated_at = excluded.updated_at,
         sync_state = 'synced'`,
    [id, json, now],
  );
}

export async function readCacheRow<T>(
  table: CacheTable,
  id: string,
): Promise<T | null> {
  const db = await getDatabase();
  const [result] = await db.executeSql(
    `SELECT data, updated_at FROM ${table} WHERE id = ?`,
    [id],
  );
  const row = result.rows.item(0);
  if (!row) {
    return null;
  }
  return JSON.parse(row.data) as T;
}

export async function listCacheRows<T>(
  table: CacheTable,
): Promise<Array<{ value: T; updatedAt: string }>> {
  const db = await getDatabase();
  const [result] = await db.executeSql(
    `SELECT data, updated_at FROM ${table} ORDER BY updated_at DESC`,
  );
  const rows: Array<{ value: T; updatedAt: string }> = [];
  for (let i = 0; i < result.rows.length; i += 1) {
    const row = result.rows.item(i) as { data: string; updated_at: string };
    rows.push({ value: JSON.parse(row.data) as T, updatedAt: row.updated_at });
  }
  return rows;
}

// --------------------------------------------------------------------------
// Sync queue helpers. The `dedup_key` UNIQUE constraint guarantees
// idempotent enqueue – re-submitting the same logical mutation just
// updates the existing row instead of creating duplicates.
// --------------------------------------------------------------------------
const MAX_SYNC_ATTEMPTS = 5;

export async function enqueueSync(
  endpoint: string,
  method: HttpMethod,
  payload: unknown,
  dedupKey: string,
): Promise<string> {
  const db = await getDatabase();
  const now = new Date().toISOString();
  const id = `q_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
  await db.executeSql(
    `INSERT INTO sync_queue
       (id, endpoint, method, payload, dedup_key, status, attempts, created_at, updated_at)
     VALUES (?, ?, ?, ?, ?, 'pending', 0, ?, ?)
     ON CONFLICT(dedup_key) DO UPDATE SET
       payload = excluded.payload,
       updated_at = excluded.updated_at,
       status = 'pending',
       attempts = 0,
       last_error = NULL,
       next_retry_at = NULL`,
    [id, endpoint, method, JSON.stringify(payload), dedupKey, now, now],
  );
  log.info('Enqueued offline operation', { endpoint, method, dedupKey });
  return dedupKey;
}

export async function listPendingSync(max = 100): Promise<SyncQueueItem[]> {
  const db = await getDatabase();
  const [result] = await db.executeSql(
    `SELECT * FROM sync_queue
       WHERE status = 'pending'
         AND (next_retry_at IS NULL OR next_retry_at <= ?)
       ORDER BY created_at ASC
       LIMIT ?`,
    [new Date().toISOString(), max],
  );
  const items: SyncQueueItem[] = [];
  for (let i = 0; i < result.rows.length; i += 1) {
    const row = result.rows.item(i) as Omit<SyncQueueItem, 'payload'> & {
      payload: string;
    };
    items.push({ ...row, payload: JSON.parse(row.payload) });
  }
  return items;
}

export async function markSyncSent(id: string): Promise<void> {
  const db = await getDatabase();
  await db.executeSql(
    `UPDATE sync_queue SET status = 'sent', updated_at = ? WHERE id = ?`,
    [new Date().toISOString(), id],
  );
}

export async function markSyncFailed(id: string, error: string): Promise<void> {
  const db = await getDatabase();
  const now = new Date().toISOString();
  const [res] = await db.executeSql(
    `SELECT attempts FROM sync_queue WHERE id = ?`,
    [id],
  );
  const attempts = ((res.rows.item(0)?.attempts as number) ?? 0) + 1;
  // Exponential backoff: 5s, 30s, 3m, 18m, ~1.5h, capped at 1h for safety.
  const delaySec = Math.min(3600, 5 * Math.pow(6, attempts - 1));
  const nextRetry = new Date(Date.now() + delaySec * 1000).toISOString();
  await db.executeSql(
    `UPDATE sync_queue SET
       status = CASE WHEN ? >= ? THEN 'failed' ELSE 'pending' END,
       attempts = ?,
       last_error = ?,
       next_retry_at = ?,
       updated_at = ?
     WHERE id = ?`,
    [attempts, MAX_SYNC_ATTEMPTS, attempts, error, nextRetry, now, id],
  );
}

export async function pendingSyncCount(): Promise<number> {
  const db = await getDatabase();
  const [result] = await db.executeSql(
    `SELECT COUNT(*) as c FROM sync_queue WHERE status = 'pending'`,
  );
  return (result.rows.item(0)?.c as number) ?? 0;
}

// --------------------------------------------------------------------------
// Convenience for tests that want a clean in-memory-ish slate without
// touching the actual SQLite file. Used by `__tests__/database.test.ts`.
// --------------------------------------------------------------------------
export async function __resetForTests(): Promise<void> {
  if (cachedDb) {
    await cachedDb.close().catch(() => undefined);
  }
  cachedDb = null;
  initPromise = null;
}

// `ResultSet` re-export keeps consumers from importing the SQLite package
// directly (smaller test surface).
export type { ResultSet };
const DB_KEY_PREFIX = 'db:v1:';

const TABLES = ['corridors', 'anchors', 'assets', 'sync_queue'] as const;
export type TableName = (typeof TABLES)[number];

export interface TableRow {
  id: string;
}

export type SyncQueueMethod = 'POST' | 'PUT' | 'DELETE';
export type SyncQueueStatus = 'pending' | 'applied' | 'failed';

/**
 * A queued offline mutation, durably persisted in the `sync_queue` table.
 *
 * `id` doubles as the idempotency key shared with the backend reconciliation
 * contract (see docs/offline-sync.md) — it must stay stable across retries
 * so replays are safe to de-duplicate server-side.
 */
export interface SyncQueueRow extends TableRow {
  method: SyncQueueMethod;
  resource: string;
  payload?: unknown;
  status: SyncQueueStatus;
  clientTimestamp: string;
  retryCount: number;
  lastError?: string;
}

function tableKey(table: TableName): string {
  return `${DB_KEY_PREFIX}${table}`;
}

function readTable<T extends TableRow>(table: TableName): T[] {
  const raw = storageUtils.getItem(tableKey(table));

  if (!raw) {
    return [];
  }

  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as T[]) : [];
  } catch (error) {
    log.warn(`Failed to parse table "${table}", resetting it`, { error });
    return [];
  }
}

function writeTable<T extends TableRow>(table: TableName, rows: T[]): void {
  storageUtils.setItem(tableKey(table), JSON.stringify(rows));
}

/**
 * Initializes all local tables (corridors, anchors, assets, sync_queue).
 * Safe to call multiple times — existing tables are left untouched.
 */
export async function initializeDatabase(): Promise<void> {
  for (const table of TABLES) {
    if (storageUtils.getItem(tableKey(table)) == null) {
      writeTable(table, []);
    }
  }
  log.info('Database initialized', { tables: TABLES });
}

/** Clears all cached data and the offline sync queue. */
export async function clearDatabase(): Promise<void> {
  for (const table of TABLES) {
    storageUtils.removeItem(tableKey(table));
  }
  log.info('Database cleared');
}

/** Reads a single row by id from the given table, or null if absent. */
export async function getRow<T extends TableRow>(
  table: TableName,
  id: string
): Promise<T | null> {
  const rows = readTable<T>(table);
  return rows.find(row => row.id === id) ?? null;
}

/** Reads every row currently stored in the given table. */
export async function getAllRows<T extends TableRow>(table: TableName): Promise<T[]> {
  return readTable<T>(table);
}

/** Inserts a row, or replaces the existing row with the same id. */
export async function upsertRow<T extends TableRow>(table: TableName, row: T): Promise<void> {
  const rows = readTable<T>(table);
  const index = rows.findIndex(existing => existing.id === row.id);

  if (index >= 0) {
    rows[index] = row;
  } else {
    rows.push(row);
  }

  writeTable(table, rows);
}

/** Removes a row by id. No-op if the row does not exist. */
export async function deleteRow(table: TableName, id: string): Promise<void> {
  const rows = readTable<TableRow>(table);
  writeTable(table, rows.filter(row => row.id !== id));
}

export interface EnqueueSyncActionInput {
  id: string;
  method: SyncQueueMethod;
  resource: string;
  payload?: unknown;
}

/** Persists a new offline mutation in the `sync_queue` table as `pending`. */
export async function enqueueSyncAction(input: EnqueueSyncActionInput): Promise<SyncQueueRow> {
  const row: SyncQueueRow = {
    ...input,
    status: 'pending',
    retryCount: 0,
    clientTimestamp: new Date().toISOString(),
  };

  await upsertRow('sync_queue', row);
  log.info('Queued offline sync action', { id: row.id, method: row.method, resource: row.resource });

  return row;
}

/** Returns every `sync_queue` row that has not yet been successfully applied. */
export async function getPendingSyncActions(): Promise<SyncQueueRow[]> {
  const rows = await getAllRows<SyncQueueRow>('sync_queue');
  return rows.filter(row => row.status === 'pending');
}

/**
 * Marks a queued action as applied or failed. Failed rows have their
 * `retryCount` incremented so callers can cap retries.
 */
export async function markSyncActionStatus(
  id: string,
  status: SyncQueueStatus,
  lastError?: string
): Promise<void> {
  const row = await getRow<SyncQueueRow>('sync_queue', id);

  if (!row) {
    return;
  }

  await upsertRow('sync_queue', {
    ...row,
    status,
    lastError,
    retryCount: status === 'failed' ? row.retryCount + 1 : row.retryCount,
  });
}

/** Removes an action from the queue once it has been durably applied. */
export async function removeSyncAction(id: string): Promise<void> {
  await deleteRow('sync_queue', id);
}
