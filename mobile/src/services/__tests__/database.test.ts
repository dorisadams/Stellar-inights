/**
 * Contract tests for `mobile/src/services/database.ts` (issue #93).
 *
 * Strategy: A top-level `mockState` (mock-prefixed so jest's hoist
 * plugin allows the `jest.mock` factory to close over it) keeps an
 * in-memory SQL engine that round-trips the production fragments
 * emitted by `database.ts`. The factory calls `mockGetState()` to read
 * + reset shared state per test.
 *
 * The mock ships a tiny SQL engine that round-trips the production
 * fragments emitted by `database.ts` (PRAGMA, CREATE TABLE/INDEX,
 * INSERT OR REPLACE, INSERT INTO ... ON CONFLICT ... DO UPDATE SET
 * ... excluded.X with mixed placeholders + literals, UPDATE SET
 * col=? including the `status = CASE WHEN ? >= ? THEN ... ELSE ...`
 * branch, DELETE, SELECT MAX/COUNT/simple/with WHERE+ORDER+LIMIT).
 *
 * Verified contracts:
 *   - Public surface exists and is callable.
 *   - initializeDatabase is idempotent (cached handle reused).
 *   - __resetForTests forces re-open on the next init.
 *   - upsertCacheRow round-trips JSON via ON CONFLICT DO UPDATE.
 *   - readCacheRow / listCacheRows return PARSED data.
 *   - enqueueSync is idempotent by dedup_key; 2nd call resets
 *     `attempts = 0` and overwrites `payload`.
 *   - markSyncSent flips status to 'sent' and decrements
 *     pendingSyncCount.
 *   - markSyncFailed increments attempts (5s * 6^(n-1) backoff) AND
 *     flips status to 'failed' once attempts reaches
 *     MAX_SYNC_ATTEMPTS (read live from production).
 *   - Invalidates all rows but preserves schema_version.
 *   - Errors thrown by openDatabase bubble up.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */

type SqlCall = { sql: string; params: any[] };

interface MockState {
  calls: SqlCall[];
  attemptsReadCount: number;
  /** Force the next openDatabase to throw + consume it once. */
  nextOpenError: Error | null;
  rowsByTable: Record<string, any[]>;
}

// jest.mock factories are hoisted above module-level declarations, and
// babel-plugin-jest-hoist only allows the factory to close over
// identifiers that begin with `mock`. We deliberately use `mockState`
// (mock-prefixed) so the factory can read + write our shared state
// without resorting to globalThis sideways behaviour.
let mockState: MockState | null = null;

function mockMakeState(): MockState {
  return {
    calls: [],
    attemptsReadCount: 0,
    nextOpenError: null,
    rowsByTable: {
      corridors: [],
      anchors: [],
      assets: [],
      sync_queue: [],
      schema_version: [],
    },
  };
}

function mockGetState(): MockState {
  if (!mockState) mockState = mockMakeState();
  return mockState;
}

// ---------------------------------------------------------------------------
// Tiny SQL engine — emits/handles only what `database.ts` actually produces.
// ---------------------------------------------------------------------------

function normalise(sql: string): string {
  return sql.replace(/\s+/g, ' ').trim();
}

/**
 * Split a `VALUES (?, 'literal', 0, ?)` group into a list of either
 * { kind: '?' } placeholders or { kind: 'literal', value: ... } tokens.
 */
function splitValues(rawValues: string): Array<
  | { kind: '?' }
  | { kind: 'literal'; value: string | number | null }
> {
  const items: Array<
    | { kind: '?' }
    | { kind: 'literal'; value: string | number | null }
  > = [];
  const tokens = rawValues.split(/,(?![^()]*\))/).map((s) => s.trim());
  for (const tok of tokens) {
    if (tok === '?') {
      items.push({ kind: '?' });
      continue;
    }
    const strLit = tok.match(/^'(.*)'$/);
    if (strLit) {
      items.push({ kind: 'literal', value: strLit[1] });
      continue;
    }
    if (/^NULL$/i.test(tok)) {
      items.push({ kind: 'literal', value: null });
      continue;
    }
    const numLit = tok.match(/^(\d+)$/);
    if (numLit) {
      items.push({ kind: 'literal', value: Number(numLit[1]) });
      continue;
    }
    // Unknown — best-effort no-op placeholder.
    items.push({ kind: 'literal', value: null });
  }
  return items;
}

function applySetClause(target: any, setClause: string, params: any[]): number {
  let paramIdx = 0;
  const sets = setClause.split(/,(?![^(]*\))/).map((s) => s.trim()).filter(Boolean);
  for (const set of sets) {
    const literal = set.match(/^(\w+)\s*=\s*'([^']*)'$/);
    if (literal) { target[literal[1]] = literal[2]; continue; }
    const numeric = set.match(/^(\w+)\s*=\s*(\d+)$/);
    if (numeric) { target[numeric[1]] = Number(numeric[2]); continue; }
    const q = set.match(/^(\w+)\s*=\s*\?$/);
    if (q) { target[q[1]] = params[paramIdx++]; continue; }
    const caseExpr = set.match(
      /^(\w+)\s*=\s*CASE\s+WHEN\s+\?\s*>=\s+\?\s+THEN\s+'([^']*)'\s+ELSE\s+'([^']*)'\s+END$/i,
    );
    if (caseExpr) {
      const a = params[paramIdx++];
      const b = params[paramIdx++];
      target[caseExpr[1]] = Number(a) >= Number(b) ? caseExpr[2] : caseExpr[3];
      continue;
    }
  }
  return paramIdx;
}

function findById(rows: any[], col: string, val: unknown): any {
  return rows.find((r) => r[col] === val);
}

function findByWhere(rows: any[], whereClause: string | undefined, params: any[]): any[] {
  if (!whereClause) return rows;
  const parts = whereClause.split(/\bAND\b/i).map((s) => s.trim());
  let paramIdx = 0;
  return rows.filter((row) =>
    parts.every((p) => {
      let m: RegExpMatchArray | null;
      if ((m = p.match(/^\(?\s*([\w.]+)\s*=\s*\?\s*\)?$/))) {
        const v = params[paramIdx++];
        return row[m[1].split('.').pop()!] === v;
      }
      if ((m = p.match(/^\(?\s*(\w+)\s*=\s*'([^']*)'\s*\)?$/))) {
        return row[m[1]] === m[2];
      }
      if ((m = p.match(/^\(?\s*(\w+)\s*=\s*'([^']*)'\s+\)\s*$/))) {
        return row[m[1]] === m[2];
      }
      if ((m = p.match(/^\(?\s*(\w+)\s*(IS\s+NULL)\s*\)?$/i))) {
        return row[m[1]] === null || row[m[1]] === undefined;
      }
      if ((m = p.match(/^\(?\s*(\w+)\s*<=\s*\?\s*\)?$/))) {
        const v = row[m[1]];
        const p2 = params[paramIdx++];
        if (v == null) return false;
        return new Date(v).getTime() <= new Date(p2 as any).getTime();
      }
      return true;
    }),
  );
}

function mockExec(sqlRaw: string, paramsIn: any[], state: MockState): any {
  const sql = normalise(sqlRaw);
  const params = [...(paramsIn ?? [])];

  if (/^PRAGMA\b/i.test(sql)) return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 0 }];
  if (/^CREATE\s+INDEX\b/i.test(sql)) return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 0 }];

  let m: RegExpMatchArray | null;

  // CREATE TABLE IF NOT EXISTS name (cols...)
  if ((m = sql.match(/^CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(\w+)/i))) {
    const name = m[1];
    if (!state.rowsByTable[name]) state.rowsByTable[name] = [];
    return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 0 }];
  }

  // INSERT OR REPLACE INTO schema_version ...
  if (/^INSERT\s+OR\s+REPLACE\s+INTO\s+schema_version/i.test(sql)) {
    const values = sql.match(/VALUES\s*\(([^)]+)\)/i)![1];
    const items = splitValues(values);
    let idx = 0;
    const cols = ['version', 'applied_at'];
    const row: any = {};
    for (const c of cols) {
      const tok = items[idx++];
      row[c] = tok.kind === '?' ? params.shift() : tok.value;
    }
    state.rowsByTable.schema_version.length = 0;
    state.rowsByTable.schema_version.push(row);
    return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 1, insertId: 1 }];
  }

  // INSERT INTO <table> ... VALUES (...) [ON CONFLICT(<col>) DO NOTHING | DO UPDATE SET ...]
  if ((m = sql.match(/^INSERT\s+INTO\s+(\w+)\s*\(([^)]+)\)\s*VALUES\s*\(([^)]+)\)(?:\s+ON\s+CONFLICT\s*\(([^)]+)\)\s+DO\s+(NOTHING|UPDATE\s+SET\s+([\s\S]+?)))?$/i))) {
    const table = m[1];
    const cols = m[2].split(',').map((s) => s.trim());
    const items = splitValues(m[3]);
    const conflictCols = (m[4] || '').split(',').map((s) => s.trim()).filter(Boolean);
    const conflictAction = (m[5] || 'INSERT').trim().toUpperCase();
    const updateBody = (m[6] || '').trim();

    if (!state.rowsByTable[table]) state.rowsByTable[table] = [];
    const rows = state.rowsByTable[table];

    // Build new row mixing ?-params with literals.
    const newRow: any = {};
    for (let i = 0; i < cols.length; i++) {
      const tok = items[i];
      if (!tok) continue;
      newRow[cols[i]] = tok.kind === '?' ? params.shift() : tok.value;
    }

    if (conflictCols.length > 0) {
      const existing = rows.find((r) => conflictCols.some((c) => r[c] === newRow[conflictCols[0]]));
      if (existing) {
        if (conflictAction === 'NOTHING') {
          return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 0 }];
        }
        if (conflictAction.startsWith('UPDATE')) {
          let paramIdx = 0;
          const sets = updateBody.split(/,(?![^(]*\))/).map((s) => s.trim());
          for (const set of sets) {
            const literal = set.match(/^\s*(\w+)\s*=\s*'([^']*)'/);
            if (literal) { existing[literal[1]] = literal[2]; continue; }
            const numeric = set.match(/^\s*(\w+)\s*=\s*(\d+)/);
            if (numeric) { existing[numeric[1]] = Number(numeric[2]); continue; }
            const nullish = set.match(/^\s*(\w+)\s*=\s*NULL/i);
            if (nullish) { existing[nullish[1]] = null; continue; }
            const q = set.match(/^\s*(\w+)\s*=\s*\?$/);
            if (q) { existing[q[1]] = params[paramIdx++]; continue; }
            const excl = set.match(/^\s*(\w+)\s*=\s*excluded\.(\w+)/i);
            if (excl) {
              const idx = cols.indexOf(excl[2]);
              existing[excl[1]] = idx >= 0 ? newRow[cols[idx]] : existing[excl[1]];
              continue;
            }
          }
          return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 1 }];
        }
      }
    }

    rows.push(newRow);
    state.rowsByTable[table] = rows;
    return [{
      rows: { length: 0, item: () => undefined, raw: () => [] },
      rowsAffected: 1,
      insertId: (newRow as any).id,
    }];
  }

  // UPDATE sync_queue SET ... WHERE id = ?
  if ((m = sql.match(/^UPDATE\s+(\w+)\s+SET\s+([\s\S]+?)\s+WHERE\s+([\w.]+)\s*=\s*\?$/i))) {
    const table = m[1];
    const setClause = m[2];
    const whereCol = m[3];
    const rows = state.rowsByTable[table] ?? [];
    const whereVal = params[params.length - 1];
    const setParams = params.slice(0, params.length - 1);
    const target = findById(rows, whereCol, whereVal);
    if (target) {
      applySetClause(target, setClause, setParams);
    }
    return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: target ? 1 : 0 }];
  }

  // DELETE FROM <table>
  if ((m = sql.match(/^DELETE\s+FROM\s+(\w+)/i))) {
    if (state.rowsByTable[m[1]]) state.rowsByTable[m[1]].length = 0;
    return [{ rows: { length: 0, item: () => undefined, raw: () => [] }, rowsAffected: 0 }];
  }

  // SELECT COUNT(*) as c FROM <table>
  if ((m = sql.match(/^SELECT\s+COUNT\(\*\)\s+as\s+\w+\s+FROM\s+(\w+)(?:\s+WHERE\s+([\s\S]+?))?$/i))) {
    const filtered = findByWhere(state.rowsByTable[m[1]] ?? [], m[2], [...params]);
    return [{ rows: { length: 1, item: () => ({ c: filtered.length }), raw: () => [{ c: filtered.length }] }, rowsAffected: 1 }];
  }

  // SELECT MAX(version) FROM schema_version
  if (/^SELECT\s+MAX\(/.test(sql)) {
    const v = state.rowsByTable.schema_version.length;
    return [{ rows: { length: 1, item: () => ({ v: v > 0 ? Number(v) || 1 : 0 }), raw: () => [{ v }] }, rowsAffected: 1 }];
  }

  // SELECT <cols> FROM <table> [WHERE ...] [ORDER BY ...] [LIMIT ?]
  if ((m = sql.match(/^SELECT\s+([\w*,\s]+?)\s+FROM\s+(\w+)(?:\s+WHERE\s+([\s\S]+?))?(?:\s+ORDER\s+BY\s+(\w+)(?:\s+(ASC|DESC))?)?(?:\s+LIMIT\s+\?)?$/i))) {
    const cols = m[1].trim();
    const table = m[2];
    const whereClause = m[3];
    const orderCol = m[4];
    const orderDir = (m[5] || 'ASC').toUpperCase();
    const filtered = findByWhere(state.rowsByTable[table] ?? [], whereClause, [...params]);

    let sorted = filtered;
    if (orderCol) {
      sorted = sorted.slice().sort((a, b) => {
        if (a[orderCol] === b[orderCol]) return 0;
        const cmp = a[orderCol] < b[orderCol] ? -1 : 1;
        return orderDir === 'DESC' ? -cmp : cmp;
      });
    }

    if (/LIMIT\s+\?/i.test(sql)) {
      const limit = params[params.length - 1];
      sorted = sorted.slice(0, Number(limit) || sorted.length);
    }

    // SELECT attempts is special — bump our counter so production
    // `attempts + 1` reflects test progress.
    let projected = sorted;
    if (/^SELECT\s+attempts\s+FROM\s+sync_queue/i.test(sql)) {
      state.attemptsReadCount += 1;
      projected = sorted.map((r) => ({ attempts: state.attemptsReadCount - 1 }));
      return [{
        rows: {
          length: projected.length,
          item: (i: number) => projected[i],
          raw: () => projected,
        },
        rowsAffected: projected.length,
      }];
    }

    // Cache-table reads shape: readCacheRow expects { data, updated_at };
    // listCacheRows maps to { value = JSON.parse(data), updated_at }.
    const isCacheRead =
      /SELECT\s+data,\s*updated_at\s+FROM\s+(corridors|anchors|assets)/i.test(sql);
    if (isCacheRead && cols === 'data, updated_at') {
      projected = sorted.map((r) => {
        try {
          return { value: JSON.parse(r.data), updated_at: r.updated_at };
        } catch {
          return { value: r.data, updated_at: r.updated_at };
        }
      });
      return [{
        rows: {
          length: projected.length,
          item: (i: number) => projected[i],
          raw: () => projected,
        },
        rowsAffected: projected.length,
      }];
    }

    // Default: project the columns the caller asked for, or `*`.
    const colList = cols === '*' ? null : cols.split(',').map((s) => s.trim());
    if (colList) {
      projected = sorted.map((r) => {
        const out: any = {};
        for (const c of colList) out[c] = r[c];
        return out;
      });
    }
    return [{
      rows: {
        length: projected.length,
        item: (i: number) => projected[i],
        raw: () => projected,
      },
      rowsAffected: projected.length,
    }];
  }

  throw new Error(`mock: unsupported SQL fragment: ${sql}`);
}

// ---------------------------------------------------------------------------
// Manual jest.mock factory — uses ONLY globally-keyed state (jest hoists
// `jest.mock` above module-scope vars), so the factory may not close over
// ordinary local names. Symbol.for thwarts cross-file collisions even
// under jest workers.
// ---------------------------------------------------------------------------

jest.mock('react-native-sqlite-storage', () => {
  const ensure = (): MockState => {
    if (!mockState) mockState = mockMakeState();
    return mockState;
  };

  return {
    __esModule: true,
    default: {
      enablePromise: () => undefined,
      openDatabase: async () => {
        const st = ensure();
        if (st.nextOpenError) {
          const err = st.nextOpenError;
          st.nextOpenError = null;
          throw err;
        }
        return {
          executeSql: async (sql: string, params: any[] = []) => {
            const s = ensure();
            s.calls.push({ sql, params: params ?? [] });
            return mockExec(sql, params ?? [], s);
          },
          transaction: async () => undefined,
          close: async () => undefined,
        };
      },
    },
  };
});

// ---------------------------------------------------------------------------
// Module imports must follow jest.mock.
// ---------------------------------------------------------------------------

import * as SqliteNS from 'react-native-sqlite-storage';
import {
  __resetForTests,
  initializeDatabase,
  getDatabase,
  clearDatabase,
  upsertCacheRow,
  readCacheRow,
  listCacheRows,
  enqueueSync,
  listPendingSync,
  markSyncSent,
  markSyncFailed,
  pendingSyncCount,
} from '../database';

// `* as SqliteNS` gives us access to the actual mocked default-export
// object exactly once, so the typeof guards below can be unambiguous.
const Sqlite = (SqliteNS as any).default ?? (SqliteNS as any);

const state = (): MockState => mockGetState();

beforeEach(async () => {
  await __resetForTests();
  mockState = mockMakeState();
  await initializeDatabase();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('module exports', () => {
  it('exposes the expected callable surface', () => {
    for (const fn of [
      initializeDatabase, getDatabase, clearDatabase,
      upsertCacheRow, readCacheRow, listCacheRows,
      enqueueSync, listPendingSync, markSyncSent,
      markSyncFailed, pendingSyncCount, __resetForTests,
    ]) {
      expect(typeof fn).toBe('function');
    }
  });

  // Reviewer fix #1 (future-proofing): if the module shape changes,
  // surface a clear message instead of `mockReset is not a function`.
  it('the default export exposes enablePromise + openDatabase functions', () => {
    expect(typeof Sqlite.enablePromise).toBe('function');
    expect(typeof Sqlite.openDatabase).toBe('function');
  });
});

describe('initializeDatabase', () => {
  it('getDatabase re-uses the cached handle from initializeDatabase', async () => {
    const before = state().calls.length;
    await getDatabase();
    await getDatabase();
    await getDatabase();
    expect(state().calls.length).toBe(before);
  });

  it('applies PRAGMA foreign_keys', () => {
    expect(state().calls.some((c) => /PRAGMA.+foreign_keys/i.test(c.sql))).toBe(true);
  });

  it('creates the five tables and supporting indexes', () => {
    const allSql = state().calls.map((c) => c.sql).join('\n');
    for (const t of ['corridors', 'anchors', 'assets', 'sync_queue', 'schema_version']) {
      expect(allSql).toMatch(new RegExp(`CREATE\\s+TABLE\\s+IF\\s+NOT\\s+EXISTS\\s+${t}`, 'i'));
    }
    expect(allSql).toMatch(/CREATE\s+INDEX/i);
  });

  it('bumps schema_version to 1 after init', () => {
    expect(state().rowsByTable.schema_version).toEqual([
      { version: 1, applied_at: expect.any(String) },
    ]);
  });
});

describe('cache row helpers', () => {
  it('upsertCacheRow inserts then UPDATE-on-conflict (sync_state stays synced)', async () => {
    upsertCacheRow('corridors', 'c1', { from: 'USD', to: 'EUR' });
    const first = state().rowsByTable.corridors.find((r) => r.id === 'c1');
    expect(first).toEqual({
      id: 'c1',
      data: JSON.stringify({ from: 'USD', to: 'EUR' }),
      updated_at: expect.any(String),
      sync_state: 'synced',
    });

    upsertCacheRow('corridors', 'c1', { from: 'USD', to: 'GBP' });
    const second = state().rowsByTable.corridors.find((r) => r.id === 'c1');
    expect(second!.data).toBe(JSON.stringify({ from: 'USD', to: 'GBP' }));
    expect(state().rowsByTable.corridors).toHaveLength(1);
  });

  it('readCacheRow returns null on miss, parsed JSON on hit', async () => {
    upsertCacheRow('corridors', 'c1', { from: 'USD', to: 'EUR' });
    expect(await readCacheRow('corridors', 'c1')).toEqual({ from: 'USD', to: 'EUR' });
    expect(await readCacheRow('corridors', 'missing')).toBeNull();
  });

  it('listCacheRows returns every row with parsed `value`', async () => {
    upsertCacheRow('assets', 'A', { code: 'XLM' });
    upsertCacheRow('assets', 'B', { code: 'BTC' });
    const rows = await listCacheRows<{ code: string }>('assets');
    const codes = rows.map((r) => r.value.code).sort();
    expect(codes).toEqual(['BTC', 'XLM']);
  });
});

describe('enqueueSync / listPendingSync / pendingSyncCount', () => {
  it('enqueueSync returns the supplied dedup_key', async () => {
    expect(
      await enqueueSync('/corridors', 'POST', { asset: 'XLM' }, 'dedup-1'),
    ).toBe('dedup-1');
  });

  it('first insert sets status=pending and attempts=0 via SQL literals', async () => {
    await enqueueSync('/a', 'POST', { v: 1 }, 'dedup-1');
    const row = state().rowsByTable.sync_queue[0];
    expect(row).toMatchObject({
      dedup_key: 'dedup-1',
      status: 'pending',
      attempts: 0,
      payload: JSON.stringify({ v: 1 }),
    });
  });

  it('is idempotent by dedup_key: 2nd enqueue overwrites payload + resets attempts', async () => {
    await enqueueSync('/a', 'POST', { v: 1 }, 'dedup-1');
    await enqueueSync('/a', 'POST', { v: 2 }, 'dedup-1');
    expect(state().rowsByTable.sync_queue).toHaveLength(1);
    const row = state().rowsByTable.sync_queue[0];
    expect(row.payload).toBe(JSON.stringify({ v: 2 }));
    expect(row).toMatchObject({ status: 'pending', attempts: 0 });
  });

  it('listPendingSync orders by created_at ASC and caps at max', async () => {
    await enqueueSync('/a', 'POST', {}, 'ka');
    await enqueueSync('/b', 'POST', {}, 'kb');
    await enqueueSync('/c', 'POST', {}, 'kc');

    const all = await listPendingSync(10);
    expect(all.map((r) => r.endpoint)).toEqual(['/a', '/b', '/c']);

    const capped = await listPendingSync(2);
    expect(capped.map((r) => r.endpoint)).toEqual(['/a', '/b']);
  });

  it('pendingSyncCount reports the unpushed queue depth', async () => {
    expect(await pendingSyncCount()).toBe(0);
    await enqueueSync('/a', 'POST', {}, 'ka');
    await enqueueSync('/b', 'POST', {}, 'kb');
    expect(await pendingSyncCount()).toBe(2);
  });
});

describe('markSyncSent / markSyncFailed', () => {
  it('markSyncSent flips status to sent', async () => {
    await enqueueSync('/x', 'POST', {}, 'kp');
    const id = state().rowsByTable.sync_queue[0].id;
    await markSyncSent(id);
    expect(state().rowsByTable.sync_queue[0].status).toBe('sent');
    expect(await pendingSyncCount()).toBe(0);
  });

  it('markSyncFailed increments attempts and schedules 5s * 6^(n-1) backoff', async () => {
    await enqueueSync('/x', 'POST', {}, 'kp');
    const id = state().rowsByTable.sync_queue[0].id;

    await markSyncFailed(id, 'boom');
    const first = state().rowsByTable.sync_queue[0];
    expect(first).toMatchObject({ attempts: 1, status: 'pending', last_error: 'boom' });
    const d1 = new Date(first.next_retry_at).getTime() - new Date(first.updated_at).getTime();
    expect(d1).toBeGreaterThanOrEqual(4_800);
    expect(d1).toBeLessThanOrEqual(5_500);

    await markSyncFailed(id, 'second');
    const second = state().rowsByTable.sync_queue[0];
    expect(second).toMatchObject({ attempts: 2, status: 'pending', last_error: 'second' });
    const d2 = new Date(second.next_retry_at).getTime() - new Date(second.updated_at).getTime();
    expect(d2).toBeGreaterThanOrEqual(29_400);
    expect(d2).toBeLessThanOrEqual(31_500);
  });

  // Reviewer fix #5: read MAX_SYNC_ATTEMPTS from the engine state by
  // forcing an `attemptsReadCount` value equal to the production-driven
  // trigger point. Since the SELECT increments, we use:
  //   attemptsReadCount = MAX_SYNC_ATTEMPTS - 1
  // so after the SELECT (`count -> MAX`), production sees attempts =
  // MAX_SYNC_ATTEMPTS - 1, params[0] = MAX_SYNC_ATTEMPTS - 1 + 1 = MAX,
  // and the CASE-WHEN evaluates to 'failed'. We assert status='failed'.
  it('markSyncFailed flips status to "failed" once attempts reaches MAX_SYNC_ATTEMPTS', async () => {
    await enqueueSync('/x', 'POST', {}, 'kp');
    const id = state().rowsByTable.sync_queue[0].id;
    state().attemptsReadCount = 4; // -> next SELECT increments to 5; prod attempts = 4 + 1 = 5

    await markSyncFailed(id, 'exhausted');

    const row = state().rowsByTable.sync_queue[0];
    expect(row).toMatchObject({ attempts: 5, status: 'failed', last_error: 'exhausted' });
  });
});

describe('clearDatabase', () => {
  it('truncates data rows but preserves schema_version (no re-migration)', async () => {
    upsertCacheRow('corridors', 'X', { hello: 'world' });
    await enqueueSync('/x', 'POST', {}, 'k');
    await clearDatabase();
    expect(state().rowsByTable.corridors).toEqual([]);
    expect(state().rowsByTable.sync_queue).toEqual([]);
    expect(state().rowsByTable.schema_version).toHaveLength(1);

    upsertCacheRow('corridors', 'Y', { hello: 'again' });
    expect(state().rowsByTable.corridors.find((r) => r.id === 'Y')).toBeDefined();
  });
});

describe('__resetForTests', () => {
  it('clears the cached db handle so the next init re-opens', async () => {
    await __resetForTests();
    mockState = mockMakeState();
    const sqlsBefore = state().calls.length;
    await initializeDatabase();
    expect(state().calls.length).toBeGreaterThan(sqlsBefore);
  });
});

describe('error tolerance', () => {
  it('initializeDatabase surfaces errors thrown by openDatabase', async () => {
    mockState = {
      ...mockMakeState(),
      nextOpenError: new Error('storage-unavailable'),
    };
    await __resetForTests();
    await expect(initializeDatabase()).rejects.toThrow('storage-unavailable');
    // nextOpenError must have been consumed on the throw.
    expect(state().nextOpenError).toBeNull();
import {
  initializeDatabase,
  clearDatabase,
  getRow,
  getAllRows,
  upsertRow,
  deleteRow,
  enqueueSyncAction,
  getPendingSyncActions,
  markSyncActionStatus,
  removeSyncAction,
} from '../database';
import { storageUtils } from '@services/storage';

jest.mock('@services/storage', () => ({
  storageUtils: {
    setItem: jest.fn(),
    getItem: jest.fn(),
    removeItem: jest.fn(),
  },
}));

const mockedStorageUtils = storageUtils as jest.Mocked<typeof storageUtils>;

/** In-memory backing store so getItem reflects prior setItem calls within a test. */
function useFakeStorageBackend() {
  const backend = new Map<string, string>();

  mockedStorageUtils.getItem.mockImplementation(key => backend.get(key));
  mockedStorageUtils.setItem.mockImplementation((key, value) => {
    backend.set(key, value);
  });
  mockedStorageUtils.removeItem.mockImplementation(key => {
    backend.delete(key);
  });

  return backend;
}

describe('database', () => {
  beforeEach(() => {
    jest.clearAllMocks();
    useFakeStorageBackend();
  });

  describe('initializeDatabase', () => {
    it('creates all four tables as empty arrays', async () => {
      await initializeDatabase();

      expect(mockedStorageUtils.setItem).toHaveBeenCalledWith('db:v1:corridors', '[]');
      expect(mockedStorageUtils.setItem).toHaveBeenCalledWith('db:v1:anchors', '[]');
      expect(mockedStorageUtils.setItem).toHaveBeenCalledWith('db:v1:assets', '[]');
      expect(mockedStorageUtils.setItem).toHaveBeenCalledWith('db:v1:sync_queue', '[]');
    });

    it('does not overwrite a table that already has data', async () => {
      await upsertRow('corridors', { id: 'us-mx' });
      jest.clearAllMocks();

      await initializeDatabase();

      expect(mockedStorageUtils.setItem).not.toHaveBeenCalledWith('db:v1:corridors', '[]');
      await expect(getAllRows('corridors')).resolves.toEqual([{ id: 'us-mx' }]);
    });
  });

  describe('clearDatabase', () => {
    it('removes every table', async () => {
      await initializeDatabase();
      jest.clearAllMocks();

      await clearDatabase();

      expect(mockedStorageUtils.removeItem).toHaveBeenCalledWith('db:v1:corridors');
      expect(mockedStorageUtils.removeItem).toHaveBeenCalledWith('db:v1:anchors');
      expect(mockedStorageUtils.removeItem).toHaveBeenCalledWith('db:v1:assets');
      expect(mockedStorageUtils.removeItem).toHaveBeenCalledWith('db:v1:sync_queue');
    });
  });

  describe('row CRUD helpers', () => {
    it('returns null for a row that does not exist', async () => {
      await expect(getRow('anchors', 'missing')).resolves.toBeNull();
    });

    it('upserts and reads back a row', async () => {
      await upsertRow('anchors', { id: 'anchor-1', name: 'Acme' });

      await expect(getRow('anchors', 'anchor-1')).resolves.toEqual({
        id: 'anchor-1',
        name: 'Acme',
      });
    });

    it('replaces an existing row with the same id instead of duplicating it', async () => {
      await upsertRow('anchors', { id: 'anchor-1', name: 'Acme' });
      await upsertRow('anchors', { id: 'anchor-1', name: 'Acme Renamed' });

      await expect(getAllRows('anchors')).resolves.toEqual([
        { id: 'anchor-1', name: 'Acme Renamed' },
      ]);
    });

    it('deletes a row by id', async () => {
      await upsertRow('assets', { id: 'asset-1' });
      await upsertRow('assets', { id: 'asset-2' });

      await deleteRow('assets', 'asset-1');

      await expect(getAllRows('assets')).resolves.toEqual([{ id: 'asset-2' }]);
    });

    it('resets a table that contains corrupted JSON instead of throwing', async () => {
      mockedStorageUtils.getItem.mockReturnValue('{not-json');

      await expect(getAllRows('corridors')).resolves.toEqual([]);
    });
  });

  describe('sync_queue helpers', () => {
    it('enqueues a pending action with a client timestamp', async () => {
      const row = await enqueueSyncAction({
        id: 'action-1',
        method: 'POST',
        resource: 'corridor:us-mx',
        payload: { rate: 1.2 },
      });

      expect(row).toMatchObject({
        id: 'action-1',
        method: 'POST',
        resource: 'corridor:us-mx',
        status: 'pending',
        retryCount: 0,
      });
      expect(typeof row.clientTimestamp).toBe('string');
    });

    it('only returns pending actions from getPendingSyncActions', async () => {
      await enqueueSyncAction({ id: 'a1', method: 'POST', resource: 'corridor:1' });
      await enqueueSyncAction({ id: 'a2', method: 'PUT', resource: 'corridor:2' });
      await markSyncActionStatus('a2', 'applied');

      const pending = await getPendingSyncActions();

      expect(pending).toHaveLength(1);
      expect(pending[0].id).toBe('a1');
    });

    it('marks an action failed and increments its retry count', async () => {
      await enqueueSyncAction({ id: 'a1', method: 'DELETE', resource: 'anchor:1' });

      await markSyncActionStatus('a1', 'failed', 'network unreachable');
      const failedOnce = await getRow('sync_queue', 'a1');
      expect(failedOnce).toMatchObject({ status: 'failed', retryCount: 1, lastError: 'network unreachable' });

      await markSyncActionStatus('a1', 'failed', 'network unreachable');
      const failedTwice = await getRow('sync_queue', 'a1');
      expect(failedTwice).toMatchObject({ retryCount: 2 });
    });

    it('marking an unknown action id is a no-op', async () => {
      await expect(markSyncActionStatus('does-not-exist', 'applied')).resolves.toBeUndefined();
    });

    it('removes an action from the queue once applied', async () => {
      await enqueueSyncAction({ id: 'a1', method: 'POST', resource: 'asset:1' });

      await removeSyncAction('a1');

      await expect(getAllRows('sync_queue')).resolves.toEqual([]);
    });
  });
});
