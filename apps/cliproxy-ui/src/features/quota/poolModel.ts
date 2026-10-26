import { HOUR_MS } from './quotaTimelineModel';
import type { QuotaStatsPoint, QuotaStatsSample } from '@/services/api/quotaStats';

export type PoolSample = Pick<QuotaStatsSample, 'at' | 'remaining' | 'resetAt'> & {
  window?: string;
};
export type PoolHistory = Record<string, PoolSample[]>;
export type PoolPoint = QuotaStatsPoint;
export type PoolMetric = 'remaining' | 'drain' | 'burn';

/** Historical rate series derived only from management-provided quota samples.
 * Current metrics and remaining projections come directly from the pools endpoint. */
export function consumptionRate(samples: PoolSample[], now: number, hours: number): number | null {
  const recent = samples.filter((sample) => sample.at >= now - hours * HOUR_MS && sample.at <= now);
  let consumed = 0;
  let elapsed = 0;
  for (let i = 1; i < recent.length; i++) {
    const before = recent[i - 1];
    const after = recent[i];
    if (
      before.resetAt !== after.resetAt ||
      before.window !== after.window ||
      after.remaining > before.remaining
    )
      continue;
    consumed += before.remaining - after.remaining;
    elapsed += after.at - before.at;
  }
  return elapsed >= 5 * 60_000 ? consumed / (elapsed / HOUR_MS) : null;
}

/** Calendar arithmetic keeps the day labels aligned across DST boundaries. */
export function poolSpan(now: number, offset: number) {
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  start.setDate(start.getDate() - 3 + offset * 7);
  const days = Array.from({ length: 15 }, (_, i) => {
    const day = new Date(start);
    day.setDate(day.getDate() + i);
    return day.getTime();
  });
  return { start: days[0], end: days[14], days };
}
