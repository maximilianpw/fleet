import { describe, expect, test } from 'bun:test';
import type { TFunction } from 'i18next';
import {
  averageQuotaRemaining,
  buildLedgerCredential,
  buildLedgerRollup,
  type LedgerCredential,
} from '@/features/quota/ledgerModel';

const t = ((key: string) => key) as unknown as TFunction;
const now = Date.parse('2099-01-01T00:00:00Z');
const hours = (value: number) => now + value * 3_600_000;

describe('buildLedgerCredential', () => {
  test('reduces Claude windows to remaining percent with reset instants', () => {
    const ledger = buildLedgerCredential(
      'claude',
      {
        status: 'success',
        planType: 'plan_max',
        windows: [
          { id: 'five-hour', label: '5h', usedPercent: 25, resetLabel: '-', resetAtMs: null },
          { id: 'seven-day', label: '7d', usedPercent: 110, resetLabel: '', resetAtMs: hours(5) },
        ],
      } as never,
      t
    );

    expect(ledger.plan).toBe('claude_quota.plan_max');
    expect(ledger.limits).toEqual([
      { key: 'five-hour', label: '5h', remaining: 75, resetAtMs: null, resetLabel: null },
      { key: 'seven-day', label: '7d', remaining: 0, resetAtMs: hours(5), resetLabel: null },
    ]);
  });

  test('non-success states contribute no limits', () => {
    expect(buildLedgerCredential('codex', { status: 'error' }, t).limits).toEqual([]);
    expect(buildLedgerCredential('codex', undefined, t).limits).toEqual([]);
  });
});

describe('buildLedgerRollup', () => {
  const credential = (limits: [string, number | null, number | null][]): LedgerCredential => ({
    plan: null,
    limits: limits.map(([key, remaining, resetAtMs]) => ({
      key,
      label: key,
      remaining,
      resetAtMs,
      resetLabel: null,
    })),
  });

  test('headlines the tightest limit that every credential reports', () => {
    const rollup = buildLedgerRollup(
      'claude',
      [
        credential([
          ['weekly', 60, hours(30)],
          ['session', 90, hours(2)],
          ['model', 5, hours(1)],
        ]),
        credential([
          ['weekly', 100, hours(10)],
          ['session', 40, hours(3)],
        ]),
      ],
      2,
      now
    );

    // `model` is lowest but only one credential reports it.
    expect(rollup.primary?.key).toBe('session');
    expect(rollup.primary?.total).toBe(130);
    expect(rollup.primary?.capacity).toBe(200);
    expect(rollup.primary?.segments).toEqual([90, 40]);
    expect(rollup.primary?.nextResetAtMs).toBe(hours(2));
    expect(rollup.secondary.map((limit) => limit.key)).toEqual(['weekly', 'model']);
  });

  test('full limits and past instants do not count as a pending reset', () => {
    const rollup = buildLedgerRollup(
      'codex',
      [credential([['weekly', 100, hours(4)]]), credential([['weekly', 20, hours(-1)]])],
      2,
      now
    );
    expect(rollup.primary?.nextResetAtMs).toBeNull();
  });

  test('unloaded credentials leave empty segments and no headline', () => {
    const rollup = buildLedgerRollup('xai', [credential([])], 0, now);
    expect(rollup.primary).toBeNull();
    expect(rollup.secondary).toEqual([]);
  });
});

test('quota average counts reporting credentials and preserves unknown state', () => {
  expect(averageQuotaRemaining({ total: 164, capacity: 200 })).toBe(82);
  expect(averageQuotaRemaining({ total: 100, capacity: 300 })).toBe(33.3);
  expect(averageQuotaRemaining({ total: 0, capacity: 100 })).toBe(0);
  expect(averageQuotaRemaining({ total: 0, capacity: 0 })).toBeNull();
});
