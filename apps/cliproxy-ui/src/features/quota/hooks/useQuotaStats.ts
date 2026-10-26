import { useEffect, useState } from 'react';
import { useQuotaStore } from '@/stores';
import {
  quotaStatsApi,
  type QuotaStatsData,
  type QuotaStatsQuery,
} from '@/services/api/quotaStats';

/** Abort superseded reads and never show one connection's data in another. */
export function useQuotaStats(query: QuotaStatsQuery, refreshKey: unknown) {
  const generation = useQuotaStore((state) => state.cacheGeneration);
  const [result, setResult] = useState<{
    query: QuotaStatsQuery;
    generation: number;
    data?: QuotaStatsData;
    error?: string;
  }>();
  useEffect(() => {
    const controller = new AbortController();
    void quotaStatsApi.read(query, controller.signal).then(
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
  }, [query, generation, refreshKey]);
  return result?.query === query && result.generation === generation ? result : undefined;
}
