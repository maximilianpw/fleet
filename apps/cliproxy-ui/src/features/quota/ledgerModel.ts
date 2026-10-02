/**
 * Quota ledger: one flat list of limits per credential, and per-provider rollups.
 *
 * The provider bodies render each state shape in full; the ledger needs the
 * opposite — every credential reduced to the same `label / remaining / reset`
 * triple so rows line up in columns and the summary strip can add them up.
 * React-free and clock-free (`now` is passed in), like quotaTimelineModel.
 */

import type { TFunction } from 'i18next';
import type {
  AntigravityQuotaState,
  ClaudeQuotaState,
  CodexQuotaState,
  DevinQuotaState,
  KimiQuotaState,
  MetaQuotaState,
  XaiQuotaState,
} from '@/types';
import {
  QUOTA_PROGRESS_HIGH_THRESHOLD,
  QUOTA_PROGRESS_MEDIUM_THRESHOLD,
} from './components/QuotaMeter';
import { formatCodexPlanLabel } from './providers/codex/data';
import type { QuotaCardState } from './providers';
import type { QuotaProviderType } from './providers/types';

export interface LedgerLimit {
  /** Stable across credentials of one provider, so rollups can group by it. */
  key: string;
  label: string;
  /** Remaining percent, 0..100; null when the provider reported no number. */
  remaining: number | null;
  resetAtMs: number | null;
  /** Pre-formatted absolute reset label, used when no instant is available. */
  resetLabel: string | null;
}

export interface LedgerCredential {
  plan: string | null;
  limits: LedgerLimit[];
}

const clampPercent = (value: number) => Math.min(100, Math.max(0, value));

const remainingFromUsed = (used: number | null | undefined): number | null =>
  typeof used === 'number' && Number.isFinite(used) ? clampPercent(100 - used) : null;

const finiteOrNull = (value: number | null | undefined): number | null =>
  typeof value === 'number' && Number.isFinite(value) ? value : null;

const cleanLabel = (value: string | null | undefined): string | null => {
  const trimmed = typeof value === 'string' ? value.trim() : '';
  return trimmed && trimmed !== '-' ? trimmed : null;
};

/** Reduce one credential's quota state to ledger limits. Non-success states yield no limits. */
export function buildLedgerCredential(
  provider: QuotaProviderType,
  quota: QuotaCardState | undefined,
  t: TFunction
): LedgerCredential {
  if (!quota || quota.status !== 'success') return { plan: null, limits: [] };

  switch (provider) {
    case 'claude': {
      const state = quota as unknown as ClaudeQuotaState;
      return {
        plan: state.planType ? t(`claude_quota.${state.planType}`) : null,
        limits: (state.windows ?? []).map((window) => ({
          key: window.id,
          label: window.labelKey ? t(window.labelKey) : window.label,
          remaining: remainingFromUsed(window.usedPercent),
          resetAtMs: finiteOrNull(window.resetAtMs),
          resetLabel: cleanLabel(window.resetLabel),
        })),
      };
    }
    case 'codex': {
      const state = quota as unknown as CodexQuotaState;
      return {
        plan: formatCodexPlanLabel(state.planType, t),
        limits: (state.windows ?? []).map((window) => ({
          key: window.id,
          label: window.labelKey ? t(window.labelKey, window.labelParams) : window.label,
          remaining: remainingFromUsed(window.usedPercent),
          resetAtMs: finiteOrNull(window.resetAtMs),
          resetLabel: cleanLabel(window.resetLabel),
        })),
      };
    }
    case 'devin': {
      const state = quota as unknown as DevinQuotaState;
      return {
        plan: cleanLabel(state.plan),
        limits: (state.windows ?? []).map((window) => ({
          key: window.id,
          label: window.label ?? t(`devin_quota.${window.id}`),
          remaining:
            window.remainingPercent === null ? null : clampPercent(window.remainingPercent),
          resetAtMs: finiteOrNull(window.resetAtMs),
          resetLabel: null,
        })),
      };
    }
    case 'xai': {
      const billing = (quota as unknown as XaiQuotaState).billing;
      if (!billing) return { plan: null, limits: [] };
      const weekly = billing.periodType === 'weekly';
      return {
        plan: billing.planLabel ?? null,
        limits:
          billing.mode === 'paid-health'
            ? []
            : [
                {
                  key: weekly ? 'weekly' : 'monthly',
                  label: t(weekly ? 'xai_quota.weekly_limit' : 'xai_quota.monthly_credits'),
                  remaining: remainingFromUsed(billing.usagePercent),
                  resetAtMs: finiteOrNull(billing.resetAtMs),
                  resetLabel: null,
                },
              ],
      };
    }
    case 'antigravity': {
      const state = quota as unknown as AntigravityQuotaState;
      return {
        plan: cleanLabel(state.subscription?.tierName) ?? cleanLabel(state.subscription?.plan),
        limits: (state.groups ?? []).flatMap((group) =>
          (group.buckets ?? []).map((bucket) => ({
            key: `${group.id}:${bucket.id}`,
            label: bucket.label || group.label,
            remaining:
              typeof bucket.remainingFraction === 'number'
                ? clampPercent(Math.round(bucket.remainingFraction * 100))
                : null,
            resetAtMs: finiteOrNull(bucket.resetAtMs),
            resetLabel: null,
          }))
        ),
      };
    }
    case 'kimi': {
      const state = quota as unknown as KimiQuotaState;
      return {
        plan: null,
        limits: (state.rows ?? []).map((row, index) => ({
          key: row.labelKey ?? row.label ?? `row-${index}`,
          label: row.labelKey ? t(row.labelKey, row.labelParams) : (row.label ?? ''),
          remaining:
            row.limit > 0
              ? clampPercent(Math.round(((row.limit - row.used) / row.limit) * 100))
              : null,
          resetAtMs: finiteOrNull(row.resetAtMs),
          resetLabel: null,
        })),
      };
    }
    case 'meta': {
      const data = (quota as unknown as MetaQuotaState).data;
      return {
        plan: cleanLabel(data?.planName),
        limits: (data?.windows ?? []).map((window) => ({
          key: window.id,
          label: t(`meta_quota.${window.id}`),
          remaining: remainingFromUsed(window.usedPercent),
          resetAtMs: typeof window.resetAt === 'number' ? window.resetAt * 1000 : null,
          resetLabel: null,
        })),
      };
    }
    default:
      return { plan: null, limits: [] };
  }
}

export type QuotaLevel = 'high' | 'medium' | 'low' | 'unknown';

/** Same thresholds as QuotaMeter: ≥70 green / ≥30 amber / <30 red. */
export const quotaLevel = (remaining: number | null): QuotaLevel => {
  if (remaining === null) return 'unknown';
  if (remaining >= QUOTA_PROGRESS_HIGH_THRESHOLD) return 'high';
  if (remaining >= QUOTA_PROGRESS_MEDIUM_THRESHOLD) return 'medium';
  return 'low';
};

/* ---------------------------------------------------------------- rollups */

export interface LedgerRollupLimit {
  key: string;
  label: string;
  /** Sum of remaining percents across reporting credentials. */
  total: number;
  /** 100 × number of reporting credentials. */
  capacity: number;
  /** Per-credential remaining, in credential order; null = not reported/loaded. */
  segments: (number | null)[];
  /** Soonest reset among reporting credentials that are below 100%. */
  nextResetAtMs: number | null;
}

/** Average of reporting credentials; missing reports are not counted as zero. */
export function averageQuotaRemaining(
  limit: Pick<LedgerRollupLimit, 'total' | 'capacity'>
): number | null {
  return limit.capacity > 0 ? Math.round((limit.total / limit.capacity) * 1000) / 10 : null;
}

export interface LedgerRollup {
  provider: QuotaProviderType;
  credentialCount: number;
  loadedCount: number;
  /** The tightest limit (lowest share remaining) — the headline of the summary cell. */
  primary: LedgerRollupLimit | null;
  secondary: LedgerRollupLimit[];
}

/**
 * Roll a provider's credentials up into one summary cell.
 *
 * Limits are grouped by key. Only keys reported by the most credentials are
 * eligible for the headline, so a model-scoped limit present on one account
 * can't outrank the account-wide window everyone shares.
 */
export function buildLedgerRollup(
  provider: QuotaProviderType,
  credentials: LedgerCredential[],
  loadedCount: number,
  now: number
): LedgerRollup {
  // Map iteration keeps first-seen order, which is the provider's own row order.
  const byKey = new Map<string, { label: string; values: (number | null)[]; resets: number[] }>();

  credentials.forEach((credential, index) => {
    for (const limit of credential.limits) {
      let bucket = byKey.get(limit.key);
      if (!bucket) {
        bucket = { label: limit.label, values: Array(credentials.length).fill(null), resets: [] };
        byKey.set(limit.key, bucket);
      }
      bucket.values[index] = limit.remaining;
      if (
        limit.resetAtMs !== null &&
        limit.resetAtMs > now &&
        limit.remaining !== null &&
        limit.remaining < 100
      ) {
        bucket.resets.push(limit.resetAtMs);
      }
    }
  });

  const limits: LedgerRollupLimit[] = Array.from(byKey, ([key, bucket]) => {
    const reported = bucket.values.filter((value): value is number => value !== null);
    return {
      key,
      label: bucket.label,
      total: Math.round(reported.reduce((sum, value) => sum + value, 0)),
      capacity: reported.length * 100,
      segments: bucket.values,
      nextResetAtMs: bucket.resets.length > 0 ? Math.min(...bucket.resets) : null,
    };
  });

  const maxReporting = Math.max(0, ...limits.map((limit) => limit.capacity));
  const share = (limit: LedgerRollupLimit) =>
    limit.capacity > 0 ? limit.total / limit.capacity : Number.POSITIVE_INFINITY;
  const primary =
    limits
      .filter((limit) => limit.capacity === maxReporting && limit.capacity > 0)
      .reduce<LedgerRollupLimit | null>(
        (best, limit) => (best === null || share(limit) < share(best) ? limit : best),
        null
      ) ?? null;

  return {
    provider,
    credentialCount: credentials.length,
    loadedCount,
    primary,
    secondary: limits.filter((limit) => limit !== primary && limit.capacity > 0),
  };
}
