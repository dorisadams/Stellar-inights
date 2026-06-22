/**
 * Mobile dev-only diagnostic service (issue #104).
 *
 * Surfaces offline-queue depth, sync status, notification health, and
 * connectivity from a single, gated entrypoint. The service refuses to
 * return any data in production builds.
 *
 * Usage:
 *   ```ts
 *   const snap = await getMobileDebugSnapshot();
 *   logger.debug('mobile debug', snap);
 *   ```
 *
 * Note: Firebase messaging is imported directly because the
 * notifications service intentionally doesn't re-export it; importing
 * from a barrel would create a circular dependency through
 * `@services/notifications` -> `@react-native-firebase/messaging` ->
 * `@services/notifications` (ts resolution can pick either side).
 */

import NetInfo, { NetInfoState } from '@react-native-community/netinfo';
import messaging from '@react-native-firebase/messaging';
import { createScopedLogger } from '@/services/logger';
import {
  pendingSyncCount,
  getDatabase,
} from '@/services/database';

const log = createScopedLogger('DebugService');

export interface MobileDebugSnapshot {
  /** Always `false` in production. */
  devMode: boolean;
  collectedAt: string;
  connectivity: {
    online: boolean;
    type: string;
    details: NetInfoState | null;
  };
  syncQueue: {
    pending: number;
    oldestPendingAt: string | null;
  };
  database: {
    initialized: boolean;
  };
  notifications: {
    permissionGranted: boolean | null;
  };
  docsUrl: string;
}

const DOCS_URL =
  'https://github.com/Stellar-Insightss/Stellar-inights/blob/main/docs/debugging-guide.md';

const DISABLED: MobileDebugSnapshot = {
  devMode: false,
  collectedAt: new Date(0).toISOString(),
  connectivity: { online: false, type: 'unknown', details: null },
  syncQueue: { pending: 0, oldestPendingAt: null },
  database: { initialized: false },
  notifications: { permissionGranted: null },
  docsUrl: DOCS_URL,
};

/** True only when the runtime permits debug surfaces. */
export function isMobileDebugEnabled(): boolean {
  // `__DEV__` is the global `boolean` injected by Metro for debug builds.
  // Production builds replace this with `false`.
  return typeof __DEV__ !== 'undefined' && __DEV__ === true;
}

export async function getMobileDebugSnapshot(): Promise<MobileDebugSnapshot> {
  if (!isMobileDebugEnabled()) {
    log.debug('mobile debug snapshot requested in production; returning disabled payload');
    return DISABLED;
  }

  let connectivity: MobileDebugSnapshot['connectivity'] = {
    online: false,
    type: 'unknown',
    details: null,
  };
  try {
    const netInfo = await NetInfo.fetch();
    connectivity = {
      online: Boolean(netInfo.isConnected),
      type: netInfo.type ?? 'unknown',
      details: netInfo,
    };
  } catch (error) {
    log.warn('NetInfo.fetch failed', { error });
  }

  let pending = 0;
  let oldestPendingAt: string | null = null;
  try {
    pending = await pendingSyncCount();
    if (pending > 0) {
      const db = await getDatabase();
      const [result] = await db.executeSql(
        `SELECT MIN(created_at) as oldest FROM sync_queue WHERE status = 'pending'`,
      );
      oldestPendingAt =
        (result.rows.item(0)?.oldest as string | null) ?? null;
    }
  } catch (error) {
    log.warn('Sync queue inspection failed', { error });
  }

  let permissionGranted: boolean | null = null;
  try {
    const status = await messaging().hasPermission();
    permissionGranted =
      status === messaging.AuthorizationStatus.AUTHORIZED ||
      status === messaging.AuthorizationStatus.PROVISIONAL;
  } catch (error) {
    log.warn('Notification permission check failed', { error });
  }

  return {
    devMode: true,
    collectedAt: new Date().toISOString(),
    connectivity,
    syncQueue: { pending, oldestPendingAt },
    database: { initialized: true },
    notifications: { permissionGranted },
    docsUrl: DOCS_URL,
 * Mobile debug diagnostics service.
 *
 * All exported functions are no-ops (return null) in production builds
 * (`__DEV__` is false). They expose non-sensitive runtime state useful for
 * local troubleshooting of offline queue, sync, and notification health.
 *
 * Usage (Flipper / Metro console):
 *   import { getFullDiagnosticReport } from '@services/debugService';
 *   getFullDiagnosticReport().then(console.log);
 */

import { Platform } from 'react-native';
import NetInfo, { NetInfoState } from '@react-native-community/netinfo';
import { storageUtils } from '@services/storage';

const OFFLINE_QUEUE_STORAGE_KEY = 'offline-queue:v1';

// ── Types ────────────────────────────────────────────────────────────────────

export interface OfflineQueueDiagnostics {
  totalItems: number;
  pendingCount: number;
  processingCount: number;
  failedCount: number;
  oldestItemAge: number | null; // ms since createdAt of the oldest item
}

export interface SyncStatus {
  isOnline: boolean;
  connectionType: string | null;
  isInternetReachable: boolean | null;
  lastCheckedAt: string;
}

export interface NotificationHealth {
  /** Always null on platforms where Notifee is unavailable (e.g. web/sim) */
  channelCreated: boolean | null;
  platform: string;
  devMode: boolean;
}

export interface MobileDiagnosticReport {
  timestamp: string;
  platform: string;
  isDev: boolean;
  offlineQueue: OfflineQueueDiagnostics;
  sync: SyncStatus;
  notifications: NotificationHealth;
}

// ── Internal helpers ──────────────────────────────────────────────────────────

function readQueueItems(): Array<{
  id: string;
  status: string;
  createdAt: string;
}> {
  try {
    const raw = storageUtils.getItem(OFFLINE_QUEUE_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw as string);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Returns offline queue diagnostic counters.
 * Returns null in production.
 */
export function getOfflineQueueDiagnostics(): OfflineQueueDiagnostics | null {
  if (!__DEV__) return null;

  const items = readQueueItems();
  const now = Date.now();

  const pending = items.filter(i => i.status === 'pending').length;
  const processing = items.filter(i => i.status === 'processing').length;
  const failed = items.filter(i => i.status === 'failed').length;

  const oldest =
    items.length > 0
      ? Math.min(...items.map(i => new Date(i.createdAt).getTime()))
      : null;

  return {
    totalItems: items.length,
    pendingCount: pending,
    processingCount: processing,
    failedCount: failed,
    oldestItemAge: oldest !== null ? now - oldest : null,
  };
}

/**
 * Returns the current network / sync status.
 * Returns null in production.
 */
export async function getSyncStatus(): Promise<SyncStatus | null> {
  if (!__DEV__) return null;

  let state: NetInfoState;
  try {
    state = await NetInfo.fetch();
  } catch {
    return {
      isOnline: false,
      connectionType: null,
      isInternetReachable: null,
      lastCheckedAt: new Date().toISOString(),
    };
  }

  return {
    isOnline: !!(state.isConnected && state.isInternetReachable),
    connectionType: state.type ?? null,
    isInternetReachable: state.isInternetReachable ?? null,
    lastCheckedAt: new Date().toISOString(),
  };
}

/**
 * Returns notification channel health information.
 * Returns null in production.
 *
 * Notifee is loaded via require() to gracefully degrade in environments where
 * the native module is unavailable (e.g. unit test runner, web simulators).
 */
export async function getNotificationHealth(): Promise<NotificationHealth | null> {
  if (!__DEV__) return null;

  let channelCreated: boolean | null = null;

  if (Platform.OS === 'android') {
    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      const notifee = require('@notifee/react-native').default as {
        getChannels: () => Promise<Array<{ id: string }>>;
      };
      const channels = await notifee.getChannels();
      channelCreated = channels.some(ch => ch.id === 'default');
    } catch {
      channelCreated = null;
    }
  }

  return {
    channelCreated,
    platform: Platform.OS,
    devMode: __DEV__,
  };
}

/**
 * Aggregates all diagnostic information into a single report object.
 * Returns null in production.
 */
export async function getFullDiagnosticReport(): Promise<MobileDiagnosticReport | null> {
  if (!__DEV__) return null;

  const [syncStatus, notifHealth] = await Promise.all([
    getSyncStatus(),
    getNotificationHealth(),
  ]);

  return {
    timestamp: new Date().toISOString(),
    platform: Platform.OS,
    isDev: __DEV__,
    offlineQueue: getOfflineQueueDiagnostics() ?? {
      totalItems: 0,
      pendingCount: 0,
      processingCount: 0,
      failedCount: 0,
      oldestItemAge: null,
    },
    sync: syncStatus ?? {
      isOnline: false,
      connectionType: null,
      isInternetReachable: null,
      lastCheckedAt: new Date().toISOString(),
    },
    notifications: notifHealth ?? {
      channelCreated: null,
      platform: Platform.OS,
      devMode: __DEV__,
    },
  };
}
