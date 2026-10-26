import { afterEach, describe, expect, test } from 'bun:test';
import { consumptionRate } from '../src/features/quota/poolModel';
import { HOUR_MS } from '../src/features/quota/quotaTimelineModel';
import { apiClient } from '../src/services/api/client';
import { quotaStatsApi } from '../src/services/api/quotaStats';

const now = Date.UTC(2026, 9, 26);
const originalGet = apiClient.get;
afterEach(() => {
  apiClient.get = originalGet;
});

describe('quota pool data contract', () => {
  test('reads management history and pools with the selected range/rate, retaining server metrics', async () => {
    const requests: { path: string; params: unknown }[] = [];
    const history = {
      'claude:fixture-id': [
        { at: now, remaining: 74, resetAt: null, window: 'weekly', periodHours: 168 },
      ],
    };
    const pool = {
      provider: 'claude',
      remaining: 74,
      capacity: 100,
      burn: null,
      drain: null,
      accountCount: 1,
      accounts: [],
      points: [{ at: now, value: 74 }],
      projection: { available: false, points: [], refills: [], emptyAt: null, emptyAccounts: [] },
    };
    apiClient.get = (async (path, config) => {
      requests.push({ path, params: config?.params });
      return path.endsWith('/history') ? { history } : { pools: [pool] };
    }) as typeof apiClient.get;
    const range = { provider: 'claude', from: now - HOUR_MS, to: now };
    expect(await quotaStatsApi.read({ ...range, rateHours: 1, usageMultiplier: 0.5 })).toEqual({
      history,
      pools: [pool],
    });
    expect(requests).toEqual([
      {
        path: '/observability/quota/pools',
        params: { ...range, rateHours: 1, usageMultiplier: 0.5 },
      },
      { path: '/observability/quota/history', params: range },
    ]);
  });

  test('unavailable management history rejects instead of fabricating a fallback', async () => {
    apiClient.get = (async () => {
      throw new Error('ClickHouse unavailable');
    }) as typeof apiClient.get;
    await expect(
      quotaStatsApi.read({ from: now - HOUR_MS, to: now, rateHours: 3, usageMultiplier: 1 })
    ).rejects.toThrow('ClickHouse unavailable');
  });

  test('historical rates exclude refills and future data and require five minutes of evidence', () => {
    const samples = [
      { at: now - 3 * HOUR_MS, remaining: 40, resetAt: now },
      { at: now - 2 * HOUR_MS, remaining: 20, resetAt: now },
      { at: now - HOUR_MS, remaining: 100, resetAt: now + 168 * HOUR_MS },
      { at: now, remaining: 94, resetAt: now + 168 * HOUR_MS },
      { at: now + HOUR_MS, remaining: 10, resetAt: now + 168 * HOUR_MS },
    ];
    expect(consumptionRate(samples, now, 3)).toBe(13);
    expect(consumptionRate(samples, now, 1)).toBe(6);
    expect(consumptionRate(samples.slice(-2), now, 6)).toBeNull();
    const brief = [
      { at: now - 299_999, remaining: 100, resetAt: null },
      { at: now, remaining: 99, resetAt: null },
    ];
    expect(consumptionRate(brief, now, 1)).toBeNull();
    brief[0].at = now - 300_000;
    expect(consumptionRate(brief, now, 1)).toBe(12);
    expect(
      consumptionRate(
        brief.map((s, i) => ({ ...s, window: i ? 'weekly' : 'daily' })),
        now,
        1
      )
    ).toBeNull();
  });

  test('fortnight navigation uses local calendar midnights across the Paris DST transition', () => {
    // Isolate TZ: changing it in-process can leak into later Intl formatters in Bun.
    const result = Bun.spawnSync({
      cmd: [
        process.execPath,
        '-e',
        `
        import {poolSpan} from './src/features/quota/poolModel.ts';
        const now = Date.UTC(2026, 9, 26);
        const span = poolSpan(now, 0);
        console.log(JSON.stringify({
          startDate: new Date(span.start).getDate(),
          midnights: span.days.every(day => new Date(day).getHours() === 0),
          hours: (span.end - span.start) / 3600000,
          nextDate: new Date(poolSpan(now, 1).start).getDate(),
        }));
      `,
      ],
      env: { ...process.env, TZ: 'Europe/Paris' },
    });
    expect(result.exitCode).toBe(0);
    expect(JSON.parse(result.stdout.toString())).toEqual({
      startDate: 23,
      midnights: true,
      hours: 337,
      nextDate: 30,
    });
  });
});
