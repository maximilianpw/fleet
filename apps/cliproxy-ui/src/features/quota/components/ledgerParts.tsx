/**
 * Small pieces shared by the quota summary strip and the ledger rows.
 */

import { useTranslation } from 'react-i18next';
import { formatInstantShort, formatRelativeInstant } from '@/utils/quota';

export type LedgerResetProps = {
  resetAtMs: number | null;
  resetLabel?: string | null;
  now: number;
  className?: string;
  relativeClassName?: string;
};

/** `in 1 day · 10/03, 14:55`, or `No reset pending` when nothing is counting down. */
export function LedgerReset({
  resetAtMs,
  resetLabel,
  now,
  className,
  relativeClassName,
}: LedgerResetProps) {
  const { t, i18n } = useTranslation();
  const usable = resetAtMs !== null && Number.isFinite(resetAtMs) && resetAtMs > now;

  if (!usable && !resetLabel) {
    return <span className={className}>{t('quota_management.ledger_no_reset')}</span>;
  }

  return (
    <span className={className}>
      {usable && (
        <span className={relativeClassName}>
          {formatRelativeInstant(resetAtMs, now, i18n.resolvedLanguage)}
        </span>
      )}
      {usable && ' · '}
      {usable ? formatInstantShort(resetAtMs) : resetLabel}
    </span>
  );
}
