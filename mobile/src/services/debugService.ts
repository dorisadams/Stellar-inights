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
  };
}
