import { useEffect, useState } from 'react';
import { useQuotaStore } from '@/stores';
import {
  quotaStatsApi,
  type QuotaStatsData,
  type QuotaStatsQuery,
} from '@/services/api/quotaStats';

/** Abort superseded reads and never show one connection's data in another. */
export function useQuotaStats(
  query: QuotaStatsQuery,
  refreshKey: unknown,
  options?: { history?: boolean }
) {
  const withHistory = options?.history ?? true;
  const generation = useQuotaStore((state) => state.cacheGeneration);
  const [result, setResult] = useState<{
    query: QuotaStatsQuery;
    generation: number;
    data?: QuotaStatsData;
    error?: string;
  }>();
  useEffect(() => {
    const controller = new AbortController();
    void (
      withHistory
        ? quotaStatsApi.read(query, controller.signal)
        : quotaStatsApi
            .readPools(query, controller.signal)
            .then((pools): QuotaStatsData => ({ pools, history: {} }))
    ).then(
      (data) => {
        if (!controller.signal.aborted) setResult({ query, generation, data });
      },
      (error: unknown) => {
        if (!controller.signal.aborted)
          setResult({
            query,
            generation,
            error: error instanceof Error ? error.message : 'Quota history unavailable',
          });
      }
    );
    return () => controller.abort();
  }, [query, generation, refreshKey, withHistory]);
  return result?.generation === generation ? result : undefined;
}
