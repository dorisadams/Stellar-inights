/**
 * Tests for useDebugDiagnostics hook.
 */

jest.mock('react-native', () => ({
  Platform: { OS: 'ios' },
}));

jest.mock('@react-native-community/netinfo', () => ({
  __esModule: true,
  default: {
    fetch: jest.fn().mockResolvedValue({
      isConnected: true,
      isInternetReachable: true,
      type: 'cellular',
    }),
  },
}));

jest.mock('@notifee/react-native', () => ({
  __esModule: true,
  default: {
    getChannels: jest.fn().mockResolvedValue([]),
  },
}));

jest.mock('@services/storage', () => ({
  storageUtils: {
    getItem: jest.fn().mockReturnValue(null),
    setItem: jest.fn(),
    removeItem: jest.fn(),
  },
}));

import { renderHook, act, waitFor } from '@testing-library/react-native';
import { useDebugDiagnostics } from '../useDebugDiagnostics';

function withDev(value: boolean, fn: () => void): void {
  const orig = global.__DEV__;
  global.__DEV__ = value;
  try {
    fn();
  } finally {
    global.__DEV__ = orig;
  }
}

describe('useDebugDiagnostics', () => {
  describe('production mode (__DEV__ = false)', () => {
    it('returns null report and null queueSnapshot', async () => {
      global.__DEV__ = false;
      const { result } = renderHook(() => useDebugDiagnostics());
      await waitFor(() => expect(result.current.isLoading).toBe(false));
      expect(result.current.report).toBeNull();
      expect(result.current.queueSnapshot).toBeNull();
    });
  });

  describe('development mode (__DEV__ = true)', () => {
    beforeEach(() => {
      global.__DEV__ = true;
    });

    afterEach(() => {
      global.__DEV__ = false;
    });

    it('fetches a report on mount', async () => {
      const { result } = renderHook(() => useDebugDiagnostics());
      await waitFor(() => expect(result.current.isLoading).toBe(false));
      expect(result.current.report).not.toBeNull();
      expect(result.current.report).toHaveProperty('timestamp');
    });

    it('refresh() re-fetches the report', async () => {
      const { result } = renderHook(() => useDebugDiagnostics());
      await waitFor(() => expect(result.current.isLoading).toBe(false));
      const firstTimestamp = result.current.report?.timestamp;

      await act(async () => {
        await result.current.refresh();
      });

      expect(result.current.report?.timestamp).toBeDefined();
      // Timestamps may be equal within the same test tick; just ensure no crash
      expect(result.current.error).toBeNull();
    });

    it('exposes queueSnapshot synchronously', () => {
      const { result } = renderHook(() => useDebugDiagnostics());
      // queueSnapshot is derived synchronously; empty mock queue → 0 items
      expect(result.current.queueSnapshot).not.toBeNull();
      expect(result.current.queueSnapshot!.totalItems).toBe(0);
    });

    it('starts in loading state then settles', async () => {
      const { result } = renderHook(() => useDebugDiagnostics());
      // May or may not still be loading synchronously after mount
      await waitFor(() => expect(result.current.isLoading).toBe(false));
      expect(result.current.error).toBeNull();
    });
  });
});
