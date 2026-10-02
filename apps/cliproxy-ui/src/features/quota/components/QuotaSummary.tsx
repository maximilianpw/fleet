/**
 * Provider summary strip: one cell per provider on the current tab.
 *
 * Each cell shows the average remaining across reporting credentials. Separate
 * bar segments keep an exhausted account visible even when the average is high.
 */

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { ResolvedTheme } from '@/types';
import { getTypeLabel } from '@/features/authFiles/constants';
import { averageQuotaRemaining, quotaLevel, type LedgerRollup } from '../ledgerModel';
import { LedgerReset } from './ledgerParts';
import { QuotaProviderIcon } from './QuotaProviderIcon';
import styles from './QuotaSummary.module.scss';

export type QuotaSummaryProps = {
  rollups: LedgerRollup[];
  resolvedTheme: ResolvedTheme;
  now: number;
};

export function QuotaSummary({ rollups, resolvedTheme, now }: QuotaSummaryProps) {
  const { t } = useTranslation();
  if (rollups.length === 0) return null;

  return (
    <section className={styles.strip} aria-label={t('quota_management.summary_label')}>
      {rollups.map((rollup) => (
        <SummaryCell
          key={rollup.provider}
          rollup={rollup}
          resolvedTheme={resolvedTheme}
          now={now}
        />
      ))}
    </section>
  );
}

function SummaryCell({
  rollup,
  resolvedTheme,
  now,
}: {
  rollup: LedgerRollup;
  resolvedTheme: ResolvedTheme;
  now: number;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const { primary, secondary } = rollup;
  const visibleSecondary = expanded ? secondary : secondary.slice(0, 1);
  const segments = primary?.segments ?? Array<null>(rollup.credentialCount).fill(null);

  return (
    <article className={styles.cell}>
      <header className={styles.head}>
        <QuotaProviderIcon
          provider={rollup.provider}
          resolvedTheme={resolvedTheme}
          className={styles.icon}
        />
        <span className={styles.name}>{getTypeLabel(t, rollup.provider)}</span>
        <span className={styles.count}>
          {t('quota_management.summary_credentials', { count: rollup.credentialCount })}
        </span>
      </header>

      <div className={styles.label}>
        {primary?.label ??
          (rollup.loadedCount === 0
            ? t('quota_management.summary_not_loaded')
            : t('quota_management.ledger_no_limits'))}
      </div>

      <div className={styles.figure}>
        <span className={styles.value}>
          {primary ? `${averageQuotaRemaining(primary)}%` : '--'}
        </span>
        <span className={styles.capacity}>
          {t('quota_management.summary_average', {
            reported: primary ? primary.capacity / 100 : 0,
            total: rollup.credentialCount,
          })}
        </span>
      </div>

      <div className={styles.segments} aria-hidden="true">
        {segments.map((remaining, index) => (
          <span key={index} className={styles.segment} data-level={quotaLevel(remaining)}>
            <span
              className={styles.segmentFill}
              style={{ width: `${remaining === null ? 0 : Math.max(remaining, 4)}%` }}
            />
          </span>
        ))}
      </div>

      <LedgerReset
        resetAtMs={primary?.nextResetAtMs ?? null}
        now={now}
        className={styles.reset}
        relativeClassName={styles.resetRelative}
      />

      {secondary.length > 0 && (
        <footer className={styles.secondary}>
          <ul className={styles.secondaryList}>
            {visibleSecondary.map((limit) => (
              <li key={limit.key} className={styles.secondaryItem}>
                <span className={styles.secondaryLabel}>{limit.label}</span>
                <span
                  className={styles.secondaryValue}
                  title={t('quota_management.summary_average', {
                    reported: limit.capacity / 100,
                    total: rollup.credentialCount,
                  })}
                >
                  {t('quota_management.summary_average_value', {
                    value: averageQuotaRemaining(limit),
                  })}
                </span>
              </li>
            ))}
          </ul>
          {secondary.length > 1 && (
            <button
              type="button"
              className={styles.toggle}
              aria-expanded={expanded}
              onClick={() => setExpanded((value) => !value)}
            >
              {expanded
                ? t('quota_management.summary_hide')
                : t('quota_management.summary_show', { count: secondary.length - 1 })}
            </button>
          )}
        </footer>
      )}
    </article>
  );
}
