import { apiClient } from './client';

/** Management API boundary. ClickHouse credentials never enter the browser. */
export type QuotaStatsPoint = { at: number; value: number };
export type QuotaStatsSample = {
  at: number;
  remaining: number;
  resetAt: number | null;
  window: string;
  periodHours: number | null;
};
export type QuotaStatsPool = {
  provider: string;
  accountCount: number;
  capacity: number;
  remaining: number;
  burn: number | null;
  drain: number | null;
  points: QuotaStatsPoint[];
  accounts: {
    provider: string;
    account: string;
    name: string;
    at: number;
    remaining: number;
    resetAt: number | null;
    window: string;
    periodHours: number | null;
    burn: number | null;
  }[];
  projection: {
    available: boolean;
    points: QuotaStatsPoint[];
    refills: QuotaStatsPoint[];
    emptyAt: number | null;
    emptyAccounts: { name: string; at: number }[];
  };
};
export type QuotaStatsQuery = {
  provider?: string;
  account?: string;
  from: number;
  to: number;
  rateHours: 1 | 3 | 6;
  usageMultiplier: 0 | 0.5 | 1 | 2;
};
export type QuotaStatsData = {
  pools: QuotaStatsPool[];
  history: Record<string, QuotaStatsSample[]>;
};

export const quotaStatsApi = {
  async readPools(query: QuotaStatsQuery, signal?: AbortSignal): Promise<QuotaStatsPool[]> {
    const { rateHours, usageMultiplier, ...range } = query;
    const response = await apiClient.get<{ pools: QuotaStatsPool[] }>(
      '/observability/quota/pools',
      {
        params: { ...range, rateHours, usageMultiplier },
        signal,
      }
    );
    return response.pools;
  },
  async read(query: QuotaStatsQuery, signal?: AbortSignal): Promise<QuotaStatsData> {
    const range = {
      provider: query.provider,
      account: query.account,
      from: query.from,
      to: query.to,
    };
    const [pools, history] = await Promise.all([
      quotaStatsApi.readPools(query, signal),
      apiClient.get<{ history: QuotaStatsData['history'] }>('/observability/quota/history', {
        params: range,
        signal,
      }),
    ]);
    return { pools, history: history.history };
  },
};
