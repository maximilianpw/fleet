/**
 * Quota ledger: credentials as rows, limits as aligned columns, grouped by provider.
 *
 * Columns are the union of limit keys inside a group, in first-seen order, so
 * `5-hour limit` sits in the same column on every Claude row even when one
 * account doesn't report it. Row states mirror QuotaCard: idle rows are a
 * click-to-load prompt, loading rows shimmer, errors read inline.
 */

import { useTranslation } from 'react-i18next';
import { IconRefreshCw } from '@/components/ui/icons';
import type { ResolvedTheme } from '@/types';
import { resolveQuotaErrorMessage } from '@/utils/quota';
import { getQuotaCacheKey, getQuotaDisplayName } from '@/utils/quota/identity';
import { getTypeLabel } from '@/features/authFiles/constants';
import { QUOTA_ADAPTERS, type QuotaCardState } from '../providers';
import type { QuotaProviderType } from '../providers/types';
import { useClaudeResetGrants } from '../providers/claude/ClaudeResetGrants';
import { isQuotaRefreshDisabled, type QuotaFileEntry } from '../logic';
import { maskIdentity, quotaLevel, type LedgerCredential, type LedgerLimit } from '../ledgerModel';
import { LedgerReset } from './ledgerParts';
import { QuotaProviderIcon } from './QuotaProviderIcon';
import styles from './QuotaLedger.module.scss';

/** Limit tracks per row; fixed so columns line up across provider groups. Extra limits wrap. */
const LEDGER_COLUMNS = 3;

export type QuotaLedgerProps = {
  entries: QuotaFileEntry[];
  quotaFor: (entry: QuotaFileEntry) => QuotaCardState | undefined;
  ledgerFor: (entry: QuotaFileEntry) => LedgerCredential;
  /** Group rows under provider headings; off when rows are sorted across providers. */
  grouped: boolean;
  showEmails: boolean;
  resolvedTheme: ResolvedTheme;
  now: number;
  canUseActions: boolean;
  resettingQuotaName: string | null;
  onRefresh: (entry: QuotaFileEntry) => void;
  onReset: (entry: QuotaFileEntry) => void;
};

type LedgerGroup = {
  provider: QuotaProviderType | null;
  entries: QuotaFileEntry[];
  columns: string[];
};

const columnsOf = (
  entries: QuotaFileEntry[],
  ledgerFor: (entry: QuotaFileEntry) => LedgerCredential
): string[] => {
  const keys = new Set<string>();
  entries.forEach((entry) => ledgerFor(entry).limits.forEach((limit) => keys.add(limit.key)));
  return Array.from(keys);
};

export function QuotaLedger(props: QuotaLedgerProps) {
  const { t } = useTranslation();
  const { entries, ledgerFor, grouped } = props;

  const groups: LedgerGroup[] = [];
  if (grouped) {
    for (const entry of entries) {
      const last = groups[groups.length - 1];
      if (last && last.provider === entry.type) last.entries.push(entry);
      else groups.push({ provider: entry.type, entries: [entry], columns: [] });
    }
  } else {
    groups.push({ provider: null, entries, columns: [] });
  }
  // Ungrouped rows mix providers, whose limits never share a key; a single
  // union would leave every row mostly blank. Each row lays out its own.
  groups.forEach((group) => {
    group.columns = group.provider ? columnsOf(group.entries, ledgerFor) : [];
  });

  return (
    <div className={styles.ledger}>
      {groups.map((group, index) => (
        <section key={group.provider ?? `mixed-${index}`} className={styles.group}>
          {group.provider && (
            <h2 className={styles.groupTitle}>
              {getTypeLabel(t, group.provider)}
              <span className={styles.groupCount}>{group.entries.length}</span>
            </h2>
          )}
          <div className={styles.rows}>
            {group.entries.map((entry) => (
              <LedgerRow
                key={`${entry.type}:${getQuotaCacheKey(entry.file)}`}
                {...props}
                entry={entry}
                columns={group.provider ? group.columns : null}
              />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

type LedgerRowProps = QuotaLedgerProps & {
  entry: QuotaFileEntry;
  /** Shared column keys for the group, or null to lay out this row's own limits. */
  columns: string[] | null;
};

function LedgerRow(props: LedgerRowProps) {
  const {
    entry,
    columns,
    quotaFor,
    ledgerFor,
    grouped,
    showEmails,
    resolvedTheme,
    now,
    canUseActions,
    resettingQuotaName,
    onRefresh,
    onReset,
  } = props;
  const { t } = useTranslation();
  const adapter = QUOTA_ADAPTERS[entry.type];
  const quota = quotaFor(entry);
  const ledger = ledgerFor(entry);
  const status = quota?.status ?? 'idle';
  const loading = status === 'loading';
  const canRefresh = canUseActions && !entry.file.disabled;
  const resetting = resettingQuotaName === getQuotaCacheKey(entry.file);

  const claudeReset = useClaudeResetGrants(
    entry.file,
    entry.type === 'claude' && status !== 'idle',
    !canRefresh || loading || resetting,
    quota,
    () => onRefresh(entry)
  );
  const showCodexReset =
    status === 'success' &&
    Boolean(adapter.resetQuota) &&
    quota !== undefined &&
    Boolean(adapter.canResetQuota?.(quota));
  // Only offer a Claude reset when a grant is actually spendable; otherwise
  // the button is permanently disabled noise on every row.
  const showClaudeReset = entry.type === 'claude' && (!claudeReset.blocked || claudeReset.busy);

  const rawName = getQuotaDisplayName(entry.file);
  const displayName = showEmails ? rawName : maskIdentity(rawName, entry.type);
  const subtitle = ledger.plan ?? (grouped ? null : getTypeLabel(t, entry.type));

  const byKey = new Map(ledger.limits.map((limit) => [limit.key, limit]));
  const cells: (LedgerLimit | null)[] = columns
    ? columns.map((key) => byKey.get(key) ?? null)
    : ledger.limits;

  return (
    <article className={styles.row} data-disabled={entry.file.disabled ? 'true' : undefined}>
      <div className={styles.identity}>
        {!grouped && (
          <QuotaProviderIcon
            provider={entry.type}
            resolvedTheme={resolvedTheme}
            className={styles.rowIcon}
          />
        )}
        <div className={styles.identityText}>
          <span className={styles.fileName} title={showEmails ? rawName : undefined}>
            {displayName}
          </span>
          {subtitle && <span className={styles.plan}>{subtitle}</span>}
        </div>
      </div>

      <div className={styles.limits} aria-busy={loading || undefined}>
        {status === 'idle' ? (
          <button
            type="button"
            className={styles.prompt}
            onClick={() => onRefresh(entry)}
            disabled={!canRefresh}
          >
            {t(`${adapter.i18nPrefix}.idle`)}
          </button>
        ) : loading && cells.length === 0 ? (
          Array.from({ length: LEDGER_COLUMNS }, (_, index) => (
            <div key={index} className={styles.cell} aria-hidden="true">
              <span className={styles.skeletonLabel} />
              <span className={styles.track} />
              <span className={styles.skeletonReset} />
            </div>
          ))
        ) : status === 'error' ? (
          <div className={styles.error} role="alert">
            {t(`${adapter.i18nPrefix}.load_failed`, {
              message: resolveQuotaErrorMessage(
                t,
                quota?.errorStatus,
                quota?.error || t('common.unknown_error')
              ),
            })}
          </div>
        ) : cells.length === 0 ? (
          <div className={styles.prompt}>{t('quota_management.ledger_no_limits')}</div>
        ) : (
          cells.map((limit, index) =>
            limit ? (
              <LedgerCell key={limit.key} limit={limit} now={now} />
            ) : (
              <div key={`empty-${index}`} className={styles.cell} aria-hidden="true" />
            )
          )
        )}
      </div>

      <div className={styles.actions}>
        {showClaudeReset && (
          <button
            type="button"
            className={styles.action}
            disabled={claudeReset.blocked}
            onClick={claudeReset.confirm}
          >
            <IconRefreshCw size={13} className={claudeReset.busy ? styles.spinning : undefined} />
            {t(`claude_reset.${claudeReset.buttonLabel}`)}
          </button>
        )}
        {showCodexReset && (
          <button
            type="button"
            className={styles.action}
            onClick={() => onReset(entry)}
            disabled={!canRefresh || loading || resetting}
          >
            <IconRefreshCw size={13} className={resetting ? styles.spinning : undefined} />
            {t('codex_quota.reset_button')}
          </button>
        )}
        <button
          type="button"
          className={styles.action}
          onClick={() => onRefresh(entry)}
          disabled={isQuotaRefreshDisabled(canRefresh, loading, resetting || claudeReset.busy)}
          title={t('auth_files.quota_refresh_hint')}
        >
          <IconRefreshCw size={13} className={loading ? styles.spinning : undefined} />
          {t('auth_files.quota_refresh_single')}
        </button>
      </div>
    </article>
  );
}

function LedgerCell({ limit, now }: { limit: LedgerLimit; now: number }) {
  const level = quotaLevel(limit.remaining);
  const width = limit.remaining === null ? 0 : limit.remaining;

  return (
    <div className={styles.cell}>
      <div className={styles.cellHead}>
        <span className={styles.cellLabel} title={limit.label}>
          {limit.label}
        </span>
        <span className={styles.cellValue}>
          {limit.remaining === null ? '--' : `${Math.round(limit.remaining)}%`}
        </span>
      </div>
      <span className={styles.track} data-level={level}>
        <span className={styles.fill} style={{ width: `${width}%` }} />
      </span>
      <LedgerReset
        resetAtMs={limit.resetAtMs}
        resetLabel={limit.resetLabel}
        now={now}
        className={styles.cellReset}
        relativeClassName={styles.cellResetRelative}
      />
    </div>
  );
}
