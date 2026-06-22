/**
 * Development-only hook that exposes a full diagnostic report for the
 * running mobile app. Returns null in all production builds.
 *
 * Usage (development builds only):
 *   const { report, refresh, isLoading } = useDebugDiagnostics();
 */

import React from 'react';
import {
  getFullDiagnosticReport,
  getOfflineQueueDiagnostics,
  MobileDiagnosticReport,
  OfflineQueueDiagnostics,
} from '@services/debugService';

export interface UseDebugDiagnosticsResult {
  /** Full diagnostic report. Always null in production. */
  report: MobileDiagnosticReport | null;
  /** Snapshot of the offline queue counters (refreshed synchronously). */
  queueSnapshot: OfflineQueueDiagnostics | null;
  isLoading: boolean;
  error: string | null;
  /** Re-fetch the full async diagnostic report. */
  refresh: () => Promise<void>;
}

export function useDebugDiagnostics(): UseDebugDiagnosticsResult {
  const [report, setReport] = React.useState<MobileDiagnosticReport | null>(null);
  const [isLoading, setIsLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  // Offline queue can be read synchronously on every render
  const queueSnapshot = __DEV__ ? getOfflineQueueDiagnostics() : null;

  const refresh = React.useCallback(async () => {
    if (!__DEV__) return;
    setIsLoading(true);
    setError(null);
    try {
      const result = await getFullDiagnosticReport();
      setReport(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Diagnostic fetch failed');
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Auto-fetch on mount in development
  React.useEffect(() => {
    if (__DEV__) {
      refresh();
    }
  }, [refresh]);

  return { report, queueSnapshot, isLoading, error, refresh };
}
