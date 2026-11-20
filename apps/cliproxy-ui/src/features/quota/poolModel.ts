import { HOUR_MS } from './quotaTimelineModel';
import type { QuotaStatsPoint, QuotaStatsSample } from '@/services/api/quotaStats';

export type PoolSample = Pick<QuotaStatsSample, 'at' | 'remaining' | 'resetAt'> & {
  window?: string;
};
export type PoolHistory = Record<string, PoolSample[]>;
export type PoolPoint = QuotaStatsPoint;
export type PoolMetric = 'remaining' | 'drain' | 'burn';

export const POOL_REFRESH_MS = 5 * 60_000;

export function poolQueryNow(now: number): number {
  return Math.floor(now / POOL_REFRESH_MS) * POOL_REFRESH_MS;
}

const SAME_RESET_TOLERANCE_MS = 60_000;

function sameReset(left: number | null, right: number | null): boolean {
  if (left === null || right === null) return left === right;
  return Math.abs(left - right) <= SAME_RESET_TOLERANCE_MS;
}

/** Historical rate series derived only from management-provided quota samples.
 * Current metrics and remaining projections come directly from the pools endpoint.
 * Keep its rules identical to `rate` in `services/cliproxy-quota/src/quotaStats.ts`. */
export function consumptionRate(samples: PoolSample[], now: number, hours: number): number | null {
  const recent = samples.filter((sample) => sample.at >= now - hours * HOUR_MS && sample.at <= now);
  const reference = recent[recent.length - 1]?.window;
  let consumed = 0;
  let elapsed = 0;
  for (let i = 1; i < recent.length; i++) {
    const before = recent[i - 1];
    const after = recent[i];
    if (
      after.at <= before.at ||
      before.window !== after.window ||
      after.window !== reference ||
      after.remaining > before.remaining ||
      (after.remaining !== before.remaining && !sameReset(before.resetAt, after.resetAt))
    )
      continue;
    if (before.resetAt !== null && before.resetAt > before.at && before.resetAt <= after.at) continue;
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
