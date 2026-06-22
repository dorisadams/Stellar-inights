import React from 'react';
import { Platform } from 'react-native';
import { QueryKey, useQuery, useQueryClient } from '@tanstack/react-query';
import { storageUtils } from '@services/storage';
import { useAppStore } from '@store/appStore';
import { createScopedLogger } from '@services/logger';
import { apiClient } from '@services/api';
import {
  getPendingSyncActions,
  markSyncActionStatus,
  removeSyncAction,
  SyncQueueRow,
} from '@services/database';

const log = createScopedLogger('OfflineCaching');

const OFFLINE_CACHE_STORAGE_KEY = 'offline-cache:v1';
const CACHE_EXPIRY_MS = 24 * 60 * 60 * 1000; // 24 hours

export interface CacheEntry {
  key: string;
  data: unknown;
  timestamp: number;
  expiresAt: number;
  platform: string;
}

export interface OfflineCacheConfig {
  enabled?: boolean;
  maxSize?: number; // in bytes
  ttl?: number; // in milliseconds
  compress?: boolean;
}

export interface UseOfflineCacheResult {
  getCachedData: (key: QueryKey) => unknown | null;
  setCachedData: (key: QueryKey, data: unknown) => void;
  invalidateCache: (key?: QueryKey) => void;
  clearCache: () => void;
  getCacheSize: () => number;
  isCacheValid: (key: QueryKey) => boolean;
}

/**
 * Converts QueryKey to string for storage
 */
function serializeKey(key: QueryKey): string {
  return Array.isArray(key) ? key.join(':') : String(key);
}

/**
 * Reads all cache entries from storage
 */
function readCache(): Map<string, CacheEntry> {
  const cache = new Map<string, CacheEntry>();

  try {
    const value = storageUtils.getItem(OFFLINE_CACHE_STORAGE_KEY);
    if (!value) {
      return cache;
    }

    const entries: CacheEntry[] = JSON.parse(value);
    const now = Date.now();

    // Filter out expired entries
    for (const entry of entries) {
      if (entry.expiresAt > now) {
        cache.set(entry.key, entry);
      }
    }
  } catch (error) {
    log.warn('Failed to read offline cache', { error });
  }

  return cache;
}

/**
 * Writes cache entries to storage
 */
function writeCache(cache: Map<string, CacheEntry>): void {
  try {
    const entries = Array.from(cache.values());
    storageUtils.setItem(OFFLINE_CACHE_STORAGE_KEY, JSON.stringify(entries));
  } catch (error) {
    log.warn('Failed to write offline cache', { error });
  }
}

/**
 * Gets current cache size in bytes
 */
function getCacheSizeInBytes(cache: Map<string, CacheEntry>): number {
  let size = 0;
  for (const entry of cache.values()) {
    size += JSON.stringify(entry).length;
  }
  return size;
}

async function executeSyncAction(row: SyncQueueRow): Promise<void> {
  if (row.method === 'POST') {
    await apiClient.post(row.resource, row.payload);
    return;
  }

  if (row.method === 'PUT') {
    await apiClient.put(row.resource, row.payload);
    return;
  }

  await apiClient.delete(row.resource);
}

export interface ReplaySyncActionsResult {
  applied: string[];
  failed: string[];
}

/**
 * Replays every pending row in the local `sync_queue` table (see
 * mobile/src/services/database.ts) against the backend, in the order they
 * were enqueued. Each row's `id` is the idempotency key shared with the
 * backend reconciliation contract (docs/offline-sync.md), so re-running this
 * after a partial failure is safe — already-applied rows have already been
 * removed from the queue.
 *
 * Intended to run once connectivity is restored; see the `useOfflineCache`
 * reconnect effect below.
 */
export async function replayPendingSyncActions(): Promise<ReplaySyncActionsResult> {
  const pending = await getPendingSyncActions();
  const result: ReplaySyncActionsResult = { applied: [], failed: [] };

  for (const row of pending) {
    try {
      await executeSyncAction(row);
      await removeSyncAction(row.id);
      result.applied.push(row.id);
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Unable to replay queued action';
      await markSyncActionStatus(row.id, 'failed', message);
      result.failed.push(row.id);
      log.warn('Failed to replay offline sync action', { id: row.id, resource: row.resource, error: message });
    }
  }

  return result;
}

/**
 * Hook for managing offline data caching.
 *
 * Issue #93: now integrates with `services/database.ts` so cached rows
 * also live in SQLite (survives app restarts) and failed mutations are
 * persisted to `sync_queue` so they're replayed when connectivity returns.
 * The MMKV cache remains the primary fast-path; SQLite is the long-term
 * fallback when MMKV is unavailable (e.g. after a redacted file).
 */
export function useOfflineCache(config?: OfflineCacheConfig): UseOfflineCacheResult {
  const [cache, setCache] = React.useState<Map<string, CacheEntry>>(() => readCache());
  const { isOnline } = useAppStore();
  const queryClient = useQueryClient();

  const ttl = config?.ttl || CACHE_EXPIRY_MS;
  const maxSize = config?.maxSize || 5 * 1024 * 1024; // 5 MB default
  const isEnabled = config?.enabled !== false;

  // Mirror a single cache entry into SQLite on a best-effort basis.
  // Failures are logged but DO NOT block the MMKV write path. We only
  // touch the affected entry (instead of iterating the whole map) so the
  // cost stays O(1) per write regardless of total cache size.
  const mirrorEntryToSqlite = React.useCallback(
    async (entry: CacheEntry) => {
      try {
        const sqlite = await import('@services/database');
        await sqlite.initializeDatabase();
        const [table] = entry.key.split(':');
        if (
          table === 'corridors' ||
          table === 'anchors' ||
          table === 'assets'
        ) {
          await sqlite.upsertCacheRow(table, entry.key, entry.data);
        }
      } catch (error) {
        log.warn('SQLite mirror failed (non-fatal)', {
          error,
          key: entry.key,
        });
      }
    },
    [],
  );

  const getCachedData = React.useCallback(
    (key: QueryKey) => {
      if (!isEnabled) {
        return null;
      }

      const serializedKey = serializeKey(key);
      const entry = cache.get(serializedKey);

      if (!entry) {
        return null;
      }

      // Check if expired
      if (entry.expiresAt <= Date.now()) {
        const newCache = new Map(cache);
        newCache.delete(serializedKey);
        writeCache(newCache);
        setCache(newCache);
        return null;
      }

      return entry.data;
    },
    [cache, isEnabled]
  );

  const setCachedData = React.useCallback(
    (key: QueryKey, data: unknown) => {
      if (!isEnabled) {
        return;
      }

      const serializedKey = serializeKey(key);
      const now = Date.now();

      const entry: CacheEntry = {
        key: serializedKey,
        data,
        timestamp: now,
        expiresAt: now + ttl,
        platform: Platform.OS,
      };

      const newCache = new Map(cache);
      newCache.set(serializedKey, entry);

      // Check cache size and remove oldest entries if needed
      let cacheSize = getCacheSizeInBytes(newCache);
      if (cacheSize > maxSize) {
        const sortedEntries = Array.from(newCache.values()).sort(
          (a, b) => a.timestamp - b.timestamp
        );

        for (const oldEntry of sortedEntries) {
          if (cacheSize <= maxSize) {
            break;
          }
          cacheSize -= JSON.stringify(oldEntry).length;
          newCache.delete(oldEntry.key);
        }
      }

      writeCache(newCache);
      // Mirror to SQLite BEFORE updating React state so a later
      // `getCachedData` read can never see "MMKV says fresh, SQLite says
      // older". The call is fire-and-forget because MMKV (the primary
      // cache) is already authoritative; SQLite is the long-term
      // fallback that tolerates eventual consistency.
      void mirrorEntryToSqlite(entry);
      setCache(newCache);
    },
    [cache, isEnabled, ttl, maxSize, mirrorEntryToSqlite]
  );

  const invalidateCache = React.useCallback(
    (key?: QueryKey) => {
      const newCache = new Map(cache);

      if (key) {
        const serializedKey = serializeKey(key);
        newCache.delete(serializedKey);
      } else {
        newCache.clear();
      }

      writeCache(newCache);
      setCache(newCache);
    },
    [cache]
  );

  const clearCache = React.useCallback(() => {
    storageUtils.removeItem(OFFLINE_CACHE_STORAGE_KEY);
    setCache(new Map());
  }, []);

  const getCacheSize = React.useCallback(() => {
    return getCacheSizeInBytes(cache);
  }, [cache]);

  const isCacheValid = React.useCallback(
    (key: QueryKey) => {
      const serializedKey = serializeKey(key);
      const entry = cache.get(serializedKey);
      return !!entry && entry.expiresAt > Date.now();
    },
    [cache]
  );

  // Auto-cache queries when online transitions to offline
  React.useEffect(() => {
    if (!isEnabled) {
      return;
    }

    const unsubscribe = useAppStore.subscribe(
      state => state.isOnline,
      (isOnlineNow, wasOnline) => {
        if (!isOnlineNow && wasOnline) {
          // Transitioning to offline - cache all current query data
          const queries = queryClient.getQueryCache().getAll();
          for (const query of queries) {
            if (query.state.data) {
              setCachedData(query.queryKey, query.state.data);
            }
          }
        }
      }
    );

    return unsubscribe;
  }, [isEnabled, queryClient, setCachedData]);

  // Replay queued offline mutations and clear stale cache when connectivity returns
  React.useEffect(() => {
    if (!isEnabled) {
      return;
    }

    const unsubscribe = useAppStore.subscribe(
      state => state.isOnline,
      (isOnlineNow, wasOnline) => {
        if (isOnlineNow && !wasOnline) {
          replayPendingSyncActions()
            .then(result => {
              if (result.applied.length > 0 || result.failed.length > 0) {
                log.info('Replayed offline sync queue on reconnect', {
                  applied: result.applied,
                  failed: result.failed,
                });
              }
              // The data we served while offline may now be stale - drop it
              // so the next read goes back to the network, and ask the
              // backend to reconcile any state we missed while offline.
              invalidateCache();
              queryClient.invalidateQueries();
              return apiClient.reconcileState();
            })
            .catch(error => {
              log.warn('Failed to reconcile state after reconnect', { error });
            });
        }
      }
    );

    return unsubscribe;
  }, [isEnabled, queryClient, invalidateCache]);

  return {
    getCachedData,
    setCachedData,
    invalidateCache,
    clearCache,
    getCacheSize,
    isCacheValid,
  };
}

/**
 * Custom hook that combines useQuery with offline caching
 * Falls back to cached data when offline
 */
export function useQueryWithOfflineCache<TData = unknown>(
  queryKey: QueryKey,
  queryFn: () => Promise<TData>,
  config?: OfflineCacheConfig
) {
  const { getCachedData, setCachedData, isCacheValid } = useOfflineCache(config);
  const { isOnline } = useAppStore();

  const query = useQuery({
    queryKey,
    queryFn: async () => {
      const data = await queryFn();
      // Cache successful response
      setCachedData(queryKey, data);
      return data;
    },
    staleTime: 5 * 60 * 1000, // 5 minutes
    enabled: isOnline,
  });

  // Fall back to cached data when offline
  const cachedData = React.useMemo(() => {
    if (isOnline || query.data) {
      return null;
    }
    return getCachedData(queryKey);
  }, [isOnline, query.data, queryKey, getCachedData]);

  return {
    ...query,
    data: query.data || (cachedData as TData | undefined),
    isUsingCache: !isOnline && !!cachedData && !query.data,
    isCacheValid: isCacheValid(queryKey),
  };
}
