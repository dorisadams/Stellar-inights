/**
 * Tests for mobile debug diagnostics service.
 *
 * Key invariants:
 * 1. All helpers return null when __DEV__ is false (production build).
 * 2. All helpers return non-null data when __DEV__ is true (dev build).
 * 3. getFullDiagnosticReport() never surfaces secret data.
 */

jest.mock('react-native', () => ({
  Platform: { OS: 'android' },
}));

jest.mock('@react-native-community/netinfo', () => ({
  __esModule: true,
  default: {
    fetch: jest.fn().mockResolvedValue({
      isConnected: true,
      isInternetReachable: true,
      type: 'wifi',
    }),
  },
}));

jest.mock('@notifee/react-native', () => ({
  __esModule: true,
  default: {
    getChannels: jest.fn().mockResolvedValue([{ id: 'default' }]),
  },
}));

jest.mock('@services/storage', () => ({
  storageUtils: {
    getItem: jest.fn().mockReturnValue(null),
    setItem: jest.fn(),
    removeItem: jest.fn(),
  },
}));

import { storageUtils } from '@services/storage';

// Helper to set __DEV__ for a block of tests
function withDev<T>(value: boolean, fn: () => T): T {
  const orig = global.__DEV__;
  global.__DEV__ = value;
  try {
    return fn();
  } finally {
    global.__DEV__ = orig;
  }
}

async function withDevAsync<T>(value: boolean, fn: () => Promise<T>): Promise<T> {
  const orig = global.__DEV__;
  global.__DEV__ = value;
  try {
    return await fn();
  } finally {
    global.__DEV__ = orig;
  }
}

// ── Import after mocks ───────────────────────────────────────────────────────
// eslint-disable-next-line @typescript-eslint/no-require-imports
const debugService = require('../debugService') as typeof import('../debugService');

// ── Production safety ─────────────────────────────────────────────────────────

describe('production safety — returns null when __DEV__ is false', () => {
  it('getOfflineQueueDiagnostics returns null', () => {
    withDev(false, () => {
      expect(debugService.getOfflineQueueDiagnostics()).toBeNull();
    });
  });

  it('getSyncStatus returns null', async () => {
    await withDevAsync(false, async () => {
      expect(await debugService.getSyncStatus()).toBeNull();
    });
  });

  it('getNotificationHealth returns null', async () => {
    await withDevAsync(false, async () => {
      expect(await debugService.getNotificationHealth()).toBeNull();
    });
  });

  it('getFullDiagnosticReport returns null', async () => {
    await withDevAsync(false, async () => {
      expect(await debugService.getFullDiagnosticReport()).toBeNull();
    });
  });
});

// ── Development mode ──────────────────────────────────────────────────────────

describe('development mode — returns diagnostic data', () => {
  describe('getOfflineQueueDiagnostics()', () => {
    it('returns zero counts when queue is empty', () => {
      (storageUtils.getItem as jest.Mock).mockReturnValue(null);
      withDev(true, () => {
        const diag = debugService.getOfflineQueueDiagnostics();
        expect(diag).not.toBeNull();
        expect(diag!.totalItems).toBe(0);
        expect(diag!.pendingCount).toBe(0);
        expect(diag!.failedCount).toBe(0);
        expect(diag!.oldestItemAge).toBeNull();
      });
    });

    it('counts items by status', () => {
      const now = new Date().toISOString();
      (storageUtils.getItem as jest.Mock).mockReturnValue(
        JSON.stringify([
          { id: '1', status: 'pending', createdAt: now },
          { id: '2', status: 'failed', createdAt: now },
          { id: '3', status: 'processing', createdAt: now },
        ])
      );
      withDev(true, () => {
        const diag = debugService.getOfflineQueueDiagnostics();
        expect(diag!.totalItems).toBe(3);
        expect(diag!.pendingCount).toBe(1);
        expect(diag!.failedCount).toBe(1);
        expect(diag!.processingCount).toBe(1);
      });
    });

    it('computes oldestItemAge for non-empty queue', () => {
      const old = new Date(Date.now() - 10_000).toISOString();
      (storageUtils.getItem as jest.Mock).mockReturnValue(
        JSON.stringify([{ id: '1', status: 'pending', createdAt: old }])
      );
      withDev(true, () => {
        const diag = debugService.getOfflineQueueDiagnostics();
        expect(diag!.oldestItemAge).toBeGreaterThanOrEqual(9_000);
      });
    });

    it('returns zero counts on malformed storage', () => {
      (storageUtils.getItem as jest.Mock).mockReturnValue('NOT_JSON{{{');
      withDev(true, () => {
        const diag = debugService.getOfflineQueueDiagnostics();
        expect(diag!.totalItems).toBe(0);
      });
    });
  });

  describe('getSyncStatus()', () => {
    it('returns connectivity info', async () => {
      await withDevAsync(true, async () => {
        const status = await debugService.getSyncStatus();
        expect(status).not.toBeNull();
        expect(status!.isOnline).toBe(true);
        expect(status!.connectionType).toBe('wifi');
        expect(typeof status!.lastCheckedAt).toBe('string');
      });
    });
  });

  describe('getNotificationHealth()', () => {
    it('reports default channel as created on android', async () => {
      await withDevAsync(true, async () => {
        const health = await debugService.getNotificationHealth();
        expect(health).not.toBeNull();
        expect(health!.platform).toBe('android');
        expect(health!.channelCreated).toBe(true);
      });
    });
  });

  describe('getFullDiagnosticReport()', () => {
    beforeEach(() => {
      (storageUtils.getItem as jest.Mock).mockReturnValue(null);
    });

    it('returns a report with all required top-level keys', async () => {
      await withDevAsync(true, async () => {
        const report = await debugService.getFullDiagnosticReport();
        expect(report).not.toBeNull();
        expect(report).toHaveProperty('timestamp');
        expect(report).toHaveProperty('platform');
        expect(report).toHaveProperty('isDev', true);
        expect(report).toHaveProperty('offlineQueue');
        expect(report).toHaveProperty('sync');
        expect(report).toHaveProperty('notifications');
      });
    });

    it('does not expose secrets in serialized output', async () => {
      await withDevAsync(true, async () => {
        const report = await debugService.getFullDiagnosticReport();
        const json = JSON.stringify(report);
        expect(json).not.toMatch(/S[A-Z2-7]{55}/);     // Stellar secret key
        expect(json).not.toMatch(/eyJ[A-Za-z0-9_-]+/); // JWT
      });
    });
  });
});
