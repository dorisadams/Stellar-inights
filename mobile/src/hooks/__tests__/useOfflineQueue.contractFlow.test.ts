import fs from 'fs';
import path from 'path';
import { create } from 'zustand';
import { renderHook, act, waitFor } from '@testing-library/react-native';

import { useOfflineQueue } from '../useOfflineQueue';
import { apiClient } from '@services/api';
import { storageUtils } from '@services/storage';

// Shared mock fixtures (also consumed by the backend and frontend
// contract-flow regression suites, see docs/integration-testing.md).
const fixture = JSON.parse(
  fs.readFileSync(path.resolve(__dirname, '../../../../fixtures/contract-flow.json'), 'utf-8')
);

jest.mock('@services/api', () => ({
  apiClient: {
    post: jest.fn(),
    put: jest.fn(),
    delete: jest.fn(),
  },
}));

jest.mock('@services/storage', () => {
  let backingStore: Record<string, string> = {};
  return {
    storageUtils: {
      getItem: jest.fn((key: string) => backingStore[key] ?? null),
      setItem: jest.fn((key: string, value: string) => {
        backingStore[key] = value;
      }),
      removeItem: jest.fn((key: string) => {
        delete backingStore[key];
      }),
      __reset: () => {
        backingStore = {};
      },
    },
  };
});

// Backed by a real zustand store so the hook's connectivity-driven effects
// (auto-sync when `isOnline` flips true) behave exactly as they do in the app.
jest.mock('@store/appStore', () => {
  const { create } = jest.requireActual('zustand');
  return {
    useAppStore: create(() => ({
      isOnline: true,
      isSyncing: false,
      setOnlineStatus: () => {},
      setSyncStatus: () => {},
    })),
  };
});

const mockedApiClient = apiClient as jest.Mocked<typeof apiClient>;
const mockedStorageUtils = storageUtils as jest.Mocked<typeof storageUtils> & { __reset: () => void };
// eslint-disable-next-line @typescript-eslint/no-var-requires
const { useAppStore: mockedAppStore } = require('@store/appStore');

describe('useOfflineQueue contract submission flow', () => {
  beforeEach(() => {
    jest.clearAllMocks();
    mockedStorageUtils.__reset();
    mockedAppStore.setState({ isOnline: true, isSyncing: false });
  });

  it('keeps a queued contract submission pending while offline', () => {
    mockedAppStore.setState({ isOnline: false });

    const { result } = renderHook(() => useOfflineQueue());

    act(() => {
      result.current.enqueue(fixture.offlineQueueItem);
    });

    expect(result.current.items).toHaveLength(1);
    expect(result.current.items[0]).toMatchObject({
      method: fixture.offlineQueueItem.method,
      url: fixture.offlineQueueItem.url,
      payload: fixture.offlineQueueItem.payload,
      status: 'pending',
    });
    expect(mockedApiClient.post).not.toHaveBeenCalled();
  });

  it('automatically syncs the queued submission and updates local state once back online', async () => {
    mockedAppStore.setState({ isOnline: false });
    const { result } = renderHook(() => useOfflineQueue());

    act(() => {
      result.current.enqueue(fixture.offlineQueueItem);
    });
    expect(result.current.items).toHaveLength(1);

    mockedApiClient.post.mockResolvedValueOnce({ hash: fixture.contractSubmission.transactionHash });

    act(() => {
      mockedAppStore.setState({ isOnline: true });
    });

    await waitFor(() => expect(mockedApiClient.post).toHaveBeenCalledTimes(1));
    expect(mockedApiClient.post).toHaveBeenCalledWith(
      fixture.offlineQueueItem.url,
      fixture.offlineQueueItem.payload
    );

    await waitFor(() => expect(result.current.items).toHaveLength(0));
  });

  it('retains a failed contract submission with retry metadata, then clears it on manual retry', async () => {
    const { result } = renderHook(() => useOfflineQueue());

    mockedApiClient.post.mockRejectedValueOnce(new Error('Network unavailable'));

    act(() => {
      result.current.enqueue(fixture.offlineQueueItem);
    });

    await act(async () => {
      await result.current.processQueue();
    });

    expect(result.current.items[0]).toMatchObject({
      status: 'failed',
      retryCount: 1,
      lastError: 'Network unavailable',
    });

    mockedApiClient.post.mockResolvedValueOnce({ hash: fixture.contractSubmission.transactionHash });

    await act(async () => {
      await result.current.retryFailed();
    });

    expect(result.current.items).toHaveLength(0);
    expect(mockedApiClient.post).toHaveBeenCalledTimes(2);
  });
});
