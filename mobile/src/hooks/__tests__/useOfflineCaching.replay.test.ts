import { replayPendingSyncActions } from '../useOfflineCaching';
import { apiClient } from '@services/api';
import {
  getPendingSyncActions,
  markSyncActionStatus,
  removeSyncAction,
  SyncQueueRow,
} from '@services/database';

jest.mock('@services/api', () => ({
  apiClient: {
    post: jest.fn(),
    put: jest.fn(),
    delete: jest.fn(),
    reconcileState: jest.fn(),
  },
}));

jest.mock('@services/database', () => ({
  getPendingSyncActions: jest.fn(),
  markSyncActionStatus: jest.fn(),
  removeSyncAction: jest.fn(),
}));

const mockedApiClient = apiClient as jest.Mocked<typeof apiClient>;
const mockedGetPending = getPendingSyncActions as jest.Mock;
const mockedMarkStatus = markSyncActionStatus as jest.Mock;
const mockedRemove = removeSyncAction as jest.Mock;

function makeRow(overrides: Partial<SyncQueueRow> = {}): SyncQueueRow {
  return {
    id: 'action-1',
    method: 'POST',
    resource: '/corridors/us-mx',
    payload: { rate: 1.2 },
    status: 'pending',
    clientTimestamp: '2026-01-01T00:00:00.000Z',
    retryCount: 0,
    ...overrides,
  };
}

describe('replayPendingSyncActions', () => {
  beforeEach(() => {
    jest.clearAllMocks();
  });

  it('does nothing when the queue is empty', async () => {
    mockedGetPending.mockResolvedValue([]);

    const result = await replayPendingSyncActions();

    expect(result).toEqual({ applied: [], failed: [] });
    expect(mockedApiClient.post).not.toHaveBeenCalled();
  });

  it('replays a POST action and removes it once applied', async () => {
    mockedGetPending.mockResolvedValue([makeRow()]);
    mockedApiClient.post.mockResolvedValue(undefined);

    const result = await replayPendingSyncActions();

    expect(mockedApiClient.post).toHaveBeenCalledWith('/corridors/us-mx', { rate: 1.2 });
    expect(mockedRemove).toHaveBeenCalledWith('action-1');
    expect(mockedMarkStatus).not.toHaveBeenCalled();
    expect(result).toEqual({ applied: ['action-1'], failed: [] });
  });

  it('dispatches PUT and DELETE methods to the matching apiClient method', async () => {
    mockedGetPending.mockResolvedValue([
      makeRow({ id: 'put-1', method: 'PUT' }),
      makeRow({ id: 'delete-1', method: 'DELETE', payload: undefined }),
    ]);
    mockedApiClient.put.mockResolvedValue(undefined);
    mockedApiClient.delete.mockResolvedValue(undefined);

    await replayPendingSyncActions();

    expect(mockedApiClient.put).toHaveBeenCalledWith('/corridors/us-mx', { rate: 1.2 });
    expect(mockedApiClient.delete).toHaveBeenCalledWith('/corridors/us-mx');
  });

  it('marks an action failed and keeps it queued when the replay throws', async () => {
    mockedGetPending.mockResolvedValue([makeRow()]);
    mockedApiClient.post.mockRejectedValue(new Error('Network unavailable'));

    const result = await replayPendingSyncActions();

    expect(mockedMarkStatus).toHaveBeenCalledWith('action-1', 'failed', 'Network unavailable');
    expect(mockedRemove).not.toHaveBeenCalled();
    expect(result).toEqual({ applied: [], failed: ['action-1'] });
  });

  it('continues replaying remaining actions after one fails', async () => {
    mockedGetPending.mockResolvedValue([
      makeRow({ id: 'fails', resource: '/anchors/1' }),
      makeRow({ id: 'succeeds', resource: '/anchors/2' }),
    ]);
    mockedApiClient.post
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce(undefined);

    const result = await replayPendingSyncActions();

    expect(result.failed).toEqual(['fails']);
    expect(result.applied).toEqual(['succeeds']);
  });
});
