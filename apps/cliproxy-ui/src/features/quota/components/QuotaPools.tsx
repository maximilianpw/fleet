import { useId, useMemo, useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import type { ResolvedTheme } from '@/types';
import { getTypeLabel } from '@/features/authFiles/constants';
import { IconChevronLeft } from '@/components/ui/icons';
import type { QuotaStatsPool, QuotaStatsQuery } from '@/services/api/quotaStats';
import { QUOTA_PROVIDER_ACCENTS, QUOTA_TAB_ORDER } from '../constants';
import { HOUR_MS, DAY_MS } from '../quotaTimelineModel';
import { useQuotaStats } from '../hooks/useQuotaStats';
import {
  consumptionRate,
  poolSpan,
  type PoolHistory,
  type PoolMetric,
  type PoolPoint,
} from '../poolModel';
import type { QuotaFileEntry } from '../logic';
import type { QuotaCardState } from '../providers';
import type { QuotaProviderType } from '../providers/types';
import { QuotaProviderIcon } from './QuotaProviderIcon';
import styles from './QuotaPools.module.scss';

const number = (value: number) => Math.round(value * 10) / 10;

export interface QuotaPoolsProps {
  entries: QuotaFileEntry[];
  quotaFor: (entry: QuotaFileEntry) => QuotaCardState | undefined;
  displayNameFor: (entry: QuotaFileEntry) => string;
  now: number;
  resolvedTheme: ResolvedTheme;
  accountsOpen: boolean;
  onToggleAccounts: () => void;
}

export function QuotaPools(props: QuotaPoolsProps) {
  const { t, i18n } = useTranslation();
  const [metric, setMetric] = useState<PoolMetric>('remaining');
  const [hours, setHours] = useState<QuotaStatsQuery['rateHours']>(3);
  const [future, setFuture] = useState<QuotaStatsQuery['usageMultiplier']>(1);
  const [offset, setOffset] = useState(0);
  const span = useMemo(() => poolSpan(props.now, offset), [props.now, offset]);
  const providers = new Set(props.entries.map((entry) => entry.type));
  const providerFilter = providers.size === 1 ? props.entries[0].type : undefined;
  const query = useMemo<QuotaStatsQuery>(
    () => ({
      provider: providerFilter,
      from: Math.max(props.now - 31 * DAY_MS, Math.min(span.start, props.now - 6 * HOUR_MS)),
      to: props.now,
      rateHours: hours,
      usageMultiplier: future,
    }),
    [providerFilter, span.start, props.now, hours, future]
  );
  const stats = useQuotaStats(query, props.quotaFor);
  const date = (at: number) =>
    new Date(at).toLocaleDateString(i18n.language, { month: 'short', day: 'numeric' });
  return (
    <>
      <div className={styles.toolbar}>
        <div
          className={styles.segment}
          role="group"
          aria-label={t('quota_management.pool_metric', { defaultValue: 'Chart metric' })}
        >
          {(['remaining', 'drain', 'burn'] as const).map((value) => (
            <button
              key={value}
              aria-pressed={metric === value}
              onClick={() => setMetric(value)}
              title={t(`quota_management.pool_${value}_hint`, {
                defaultValue:
                  value === 'remaining'
                    ? 'Sum of remaining account percentages'
                    : value === 'drain'
                      ? 'Percentage of the whole pool consumed per hour'
                      : 'Account percentage points consumed per hour',
              })}
            >
              {t(`quota_management.pool_${value}`, {
                defaultValue:
                  value === 'remaining'
                    ? 'Remaining'
                    : value === 'drain'
                      ? 'Drain rate'
                      : 'Burn rate',
              })}
            </button>
          ))}
        </div>
        <div
          className={styles.segment}
          role="group"
          aria-label={t('quota_management.pool_lookback', { defaultValue: 'Rate lookback' })}
        >
          {([1, 3, 6] as const).map((value) => (
            <button key={value} aria-pressed={hours === value} onClick={() => setHours(value)}>
              {value}h
            </button>
          ))}
        </div>
        <label className={styles.future}>
          <span>{t('quota_management.pool_future', { defaultValue: 'Future usage' })}</span>
          <select
            value={future}
            onChange={(event) =>
              setFuture(Number(event.target.value) as QuotaStatsQuery['usageMultiplier'])
            }
          >
            <option value={1}>
              {t('quota_management.pool_same', { defaultValue: 'Same as now' })}
            </option>
            <option value={0.5}>
              {t('quota_management.pool_half', { defaultValue: 'Half as much' })}
            </option>
            <option value={2}>
              {t('quota_management.pool_double', { defaultValue: 'Twice as much' })}
            </option>
            <option value={0}>
              {t('quota_management.pool_none', { defaultValue: 'No usage' })}
            </option>
          </select>
        </label>
        <button
          className={styles.accounts}
          aria-expanded={props.accountsOpen}
          aria-controls="quota-accounts"
          onClick={props.onToggleAccounts}
        >
          {t('quota_management.pool_accounts', { defaultValue: 'Accounts' })}
        </button>
        <div className={styles.navigation}>
          <span>
            {date(span.start)} {t('quota_management.pool_to', { defaultValue: 'to' })}{' '}
            {date(span.days[13])}
          </span>
          <button
            aria-label={t('quota_management.windows_prev', { defaultValue: 'Previous' })}
            onClick={() => setOffset(offset - 1)}
          >
            <IconChevronLeft size={18} />
          </button>
          <button className={styles.today} onClick={() => setOffset(0)}>
            {t('quota_management.windows_today', { defaultValue: 'Today' })}
          </button>
          <button
            aria-label={t('quota_management.windows_next', { defaultValue: 'Next' })}
            onClick={() => setOffset(offset + 1)}
          >
            <IconChevronLeft size={18} style={{ transform: 'rotate(180deg)' }} />
          </button>
        </div>
      </div>
      <div className={styles.pools} hidden={props.accountsOpen}>
        {QUOTA_TAB_ORDER.map((provider) => {
          const entries = props.entries.filter((entry) => entry.type === provider);
          if (!entries.length) return null;
          const history = stats?.data?.history ?? {};
          const pool = stats?.data?.pools.find((pool) => pool.provider === provider);
          const lanes = (pool?.accounts ?? []).map((account, index) => {
            const entry = entries.find((entry) => String(entry.file.authIndex) === account.account);
            return {
              name: account.name,
              displayName: entry
                ? props.displayNameFor(entry)
                : `${getTypeLabel(t, provider)} ${index + 1}`,
              anchorMs: account.resetAt,
            };
          });
          return (
            <PoolCard
              key={provider}
              {...props}
              provider={provider}
              lanes={lanes}
              pool={pool}
              configuredCount={entries.length}
              history={history}
              unavailable={Boolean(stats?.error)}
              loading={!stats}
              span={span}
              metric={metric}
              hours={hours}
              future={future}
            />
          );
        })}
      </div>
    </>
  );
}

function PoolCard({
  provider,
  lanes,
  history,
  now,
  span,
  metric,
  hours,
  future,
  pool,
  configuredCount,
  unavailable,
  loading,
  resolvedTheme,
}: QuotaPoolsProps & {
  provider: QuotaProviderType;
  lanes: { name: string; displayName: string; anchorMs: number | null }[];
  history: PoolHistory;
  pool?: QuotaStatsPool;
  configuredCount: number;
  unavailable: boolean;
  loading: boolean;
  span: ReturnType<typeof poolSpan>;
  metric: PoolMetric;
  hours: number;
  future: number;
}) {
  const { t, i18n } = useTranslation();
  const id = useId().replace(/:/g, '');
  const [hover, setHover] = useState<PoolPoint | null>(null);
  const capacity = pool?.capacity ?? 100;
  const burn = pool?.burn ?? null;
  const forecast = pool?.projection ?? {
    available: false,
    points: [],
    refills: [],
    emptyAccounts: [],
    emptyAt: null,
  };
  const recorded = useMemo(() => pool?.points ?? [], [pool]);
  const ratePoints = useMemo(
    () =>
      metric === 'remaining'
        ? []
        : recorded.flatMap((point) => {
            if (!lanes.length) return [];
            const atRates = lanes.map((lane) =>
              consumptionRate(history[lane.name] ?? [], point.at, hours)
            );
            if (atRates.some((rate) => rate === null)) return [];
            const total = atRates.reduce<number>((sum, rate) => sum + rate!, 0);
            return [{ at: point.at, value: metric === 'drain' ? total / lanes.length : total }];
          }),
    [recorded, lanes, history, hours, metric]
  );
  const recordedPoints = metric === 'remaining' ? recorded : ratePoints;
  const projectedPoints =
    metric === 'remaining'
      ? forecast.points
      : burn === null || !forecast.available
        ? []
        : [
            { at: now, value: (metric === 'drain' ? pool!.drain! : burn) * future },
            {
              at: now + 14 * 24 * HOUR_MS,
              value: (metric === 'drain' ? pool!.drain! : burn) * future,
            },
          ];
  const max =
    metric === 'remaining'
      ? capacity
      : Math.max(
          1,
          ...recordedPoints.map((point) => point.value),
          ...projectedPoints.map((point) => point.value)
        ) * 1.2;
  const x = (at: number) => 58 + ((at - span.start) / (span.end - span.start)) * 1150;
  const y = (value: number) => 260 - (value / max) * 220;
  const path = (points: PoolPoint[]) =>
    points.map((point, i) => `${i ? 'L' : 'M'}${x(point.at)},${y(point.value)}`).join(' ');
  const area = (points: PoolPoint[]) =>
    points.length
      ? `${path(points)} L${x(points[points.length - 1].at)},260 L${x(points[0].at)},260 Z`
      : '';
  const instant = (at: number) =>
    new Date(at).toLocaleString(i18n.language, {
      weekday: 'short',
      hour: 'numeric',
      minute: '2-digit',
    });
  const unknown = t('quota_management.pool_unknown', { defaultValue: 'Not enough data' });
  const nextReset = lanes
    .map((lane) => lane.anchorMs)
    .filter((at): at is number => at !== null && at > now)
    .sort((a, b) => a - b)[0];
  const nextRefill = forecast.refills.find((point) => point.at === nextReset);
  const first = forecast.emptyAccounts[0];
  const currentValue =
    metric === 'remaining'
      ? (pool?.remaining ?? null)
      : burn === null
        ? null
        : metric === 'drain'
          ? (pool?.drain ?? null)
          : burn;
  const unit = metric === 'remaining' ? '%' : '%/h';
  const visiblePoints = [...recordedPoints, ...projectedPoints].filter(
    (point) => point.at >= span.start && point.at <= span.end
  );
  const status = unavailable
    ? t('quota_management.pool_unavailable', {
        defaultValue:
          'Quota history unavailable. Check the ClickHouse quota management API on the server. Accounts still shows live credential quotas.',
      })
    : loading
      ? t('quota_management.pool_loading', { defaultValue: 'Loading quota history…' })
      : !pool
        ? t('quota_management.pool_no_history', {
            defaultValue: 'No recorded quota samples for this provider and time range.',
          })
        : !forecast.available
          ? t('quota_management.pool_collecting', {
              defaultValue: 'Not enough recorded quota samples to estimate usage and projections.',
            })
          : null;
  return (
    <section
      className={styles.card}
      style={{ '--pool-accent': QUOTA_PROVIDER_ACCENTS[provider] } as CSSProperties}
      aria-label={`${getTypeLabel(t, provider)} quota pool`}
    >
      <div className={styles.provider}>
        <QuotaProviderIcon provider={provider} resolvedTheme={resolvedTheme} size={17} />
        <h2>{getTypeLabel(t, provider)}</h2>
        <span>
          {t('quota_management.pool_account_count', {
            defaultValue: '{{count}} accounts',
            count: pool?.accountCount ?? configuredCount,
          })}
        </span>
      </div>
      <div className={styles.summary}>
        <div className={styles.balance}>
          <strong>
            {currentValue === null ? '—' : number(currentValue)}
            {currentValue !== null ? unit : ''}
          </strong>
          {metric === 'remaining' && (
            <span>
              {pool &&
                t('quota_management.pool_capacity', { defaultValue: 'of {{capacity}}%', capacity })}
            </span>
          )}
        </div>
        <dl className={styles.metrics}>
          <div>
            <dt>{t('quota_management.pool_empty', { defaultValue: 'Pool empty' })}</dt>
            <dd>
              {!forecast.available
                ? unknown
                : forecast.emptyAt !== null
                  ? instant(forecast.emptyAt)
                  : t('quota_management.pool_not_by', {
                      defaultValue: 'Not by {{date}}',
                      date: new Date(
                        forecast.points[forecast.points.length - 1].at
                      ).toLocaleDateString(i18n.language, { month: 'short', day: 'numeric' }),
                    })}
            </dd>
          </div>
          <div>
            <dt>{t('quota_management.pool_first_empty', { defaultValue: 'First empty' })}</dt>
            <dd>
              {first
                ? `${lanes.find((lane) => lane.name === first.name)?.displayName} · ${instant(first.at)}`
                : !forecast.available
                  ? unknown
                  : '—'}
            </dd>
          </div>
          <div>
            <dt>{t('quota_management.pool_next_reset', { defaultValue: 'Next reset' })}</dt>
            <dd>
              {nextReset ? (
                <>
                  {nextRefill && <b>+{number(nextRefill.value)}% </b>}
                  {instant(nextReset)}
                </>
              ) : (
                '—'
              )}
            </dd>
          </div>
          <div>
            <dt>
              {t('quota_management.pool_burn_recent', {
                defaultValue: 'Burn, last {{hours}}h',
                hours,
              })}
            </dt>
            <dd>{burn === null ? '—' : `${number(burn)}%/h`}</dd>
          </div>
        </dl>
      </div>
      <div className={styles.chart}>
        <svg
          viewBox="0 0 1220 290"
          preserveAspectRatio="none"
          role="img"
          aria-label={`${getTypeLabel(t, provider)} ${metric} — ${t('quota_management.pool_chart_description', { defaultValue: 'solid recorded observations, dashed projection' })}`}
          onPointerLeave={() => setHover(null)}
          onPointerMove={(event) => {
            const rect = event.currentTarget.getBoundingClientRect();
            const at =
              span.start +
              ((((event.clientX - rect.left) / rect.width) * 1220 - 58) / 1150) *
                (span.end - span.start);
            setHover(
              visiblePoints.reduce<PoolPoint | null>(
                (best, point) =>
                  !best || Math.abs(point.at - at) < Math.abs(best.at - at) ? point : best,
                null
              )
            );
          }}
        >
          <defs>
            <linearGradient id={`${id}-fill`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="var(--pool-accent)" stopOpacity=".22" />
              <stop offset="100%" stopColor="var(--pool-accent)" stopOpacity="0" />
            </linearGradient>
            <clipPath id={`${id}-clip`}>
              <rect x="58" y="0" width="1150" height="261" />
            </clipPath>
          </defs>
          {[0, max / 2, max].map((value) => (
            <g key={value}>
              <line x1="58" x2="1208" y1={y(value)} y2={y(value)} className={styles.gridLine} />
              <text x="49" y={y(value) + 4} textAnchor="end">
                {number(value)}
                {unit}
              </text>
            </g>
          ))}
          {span.days.slice(0, -1).map((at, index) => (
            <g key={at}>
              <line x1={x(at)} x2={x(at)} y1="40" y2="260" className={styles.dayLine} />
              <text x={x((at + span.days[index + 1]) / 2)} y="278" textAnchor="middle">
                {new Date(at).toLocaleDateString(i18n.language, { weekday: 'short' })}{' '}
                {new Date(at).getDate()}
              </text>
            </g>
          ))}
          <g clipPath={`url(#${id}-clip)`}>
            <path d={area(recordedPoints)} fill={`url(#${id}-fill)`} />
            <path d={area(projectedPoints)} fill={`url(#${id}-fill)`} opacity=".45" />
            <path d={path(recordedPoints)} className={styles.line} />
            <path d={path(projectedPoints)} className={styles.projected} />
            {metric === 'remaining' &&
              recordedPoints.map((point, index) => {
                const increase = point.value - (recordedPoints[index - 1]?.value ?? point.value);
                return increase >= 5 && point.at >= span.start && point.at < span.end ? (
                  <text
                    key={point.at}
                    x={x(point.at)}
                    y={Math.max(33, y(point.value) - 9)}
                    textAnchor="middle"
                  >
                    +{Math.round(increase)}%
                  </text>
                ) : null;
              })}
            {metric === 'remaining' &&
              forecast.refills
                .filter((refill) => refill.at >= span.start && refill.at < span.end)
                .map((refill, index) => {
                  const after = [...forecast.points]
                    .reverse()
                    .find((point) => point.at === refill.at)!;
                  return (
                    <text
                      key={refill.at}
                      x={x(refill.at)}
                      y={Math.max(33, y(after.value) - 9 - (index % 2) * 3)}
                      textAnchor="middle"
                    >
                      +{Math.round(refill.value)}%
                    </text>
                  );
                })}
          </g>
          {now >= span.start && now <= span.end && (
            <g>
              <line x1={x(now)} x2={x(now)} y1="32" y2="285" className={styles.nowLine} />
              <text x={x(now)} y="23" textAnchor="middle">
                {t('quota_management.pool_now', { defaultValue: 'Now' })}
              </text>
              {currentValue !== null && (
                <circle cx={x(now)} cy={y(currentValue)} r="4.5" className={styles.nowDot} />
              )}
            </g>
          )}
          {metric === 'remaining' &&
            forecast.emptyAt !== null &&
            forecast.emptyAt >= span.start &&
            forecast.emptyAt <= span.end && (
              <g>
                <circle cx={x(forecast.emptyAt)} cy="260" r="5" className={styles.emptyDot} />
                <text
                  x={Math.min(1100, Math.max(160, x(forecast.emptyAt)))}
                  y="249"
                  textAnchor="middle"
                  className={styles.emptyLabel}
                >
                  {t('quota_management.pool_empty_at', {
                    defaultValue: 'Empty {{date}}',
                    date: instant(forecast.emptyAt),
                  })}
                </text>
              </g>
            )}
        </svg>
        {status && <p className={styles.chartStatus}>{status}</p>}
        {hover && (
          <output className={styles.tooltip}>
            {instant(hover.at)} · {number(hover.value)}
            {unit} ·{' '}
            {hover.at > now
              ? t('quota_management.pool_projected', { defaultValue: 'Projected' })
              : t('quota_management.pool_recorded', { defaultValue: 'Recorded' })}
          </output>
        )}
      </div>
      <footer className={styles.legend}>
        <span>{t('quota_management.pool_label', { defaultValue: 'Pool' })}</span>
        <span>
          <i />
          {t('quota_management.pool_recorded', { defaultValue: 'Recorded' })}
        </span>
        <span>
          <i className={styles.dashed} />
          {t('quota_management.pool_projected', { defaultValue: 'Projected' })}
        </span>
        <span className={styles.note}>
          {pool &&
            pool.accountCount !== configuredCount &&
            t('quota_management.pool_observed_count', {
              defaultValue: '{{observed}} recorded / {{configured}} configured · ',
              observed: pool.accountCount,
              configured: configuredCount,
            })}
          {t('quota_management.pool_note', {
            defaultValue: 'ClickHouse quota history',
          })}
          {pool &&
            pool.accounts.length > 0 &&
            ` · ${t('quota_management.pool_latest', { defaultValue: 'Latest' })} ${instant(Math.max(...pool.accounts.map((account) => account.at)))}`}
        </span>
      </footer>
    </section>
  );
}
