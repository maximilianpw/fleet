import { useMemo, useState, type CSSProperties } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { useAuthStore, useThemeStore } from '@/stores';
import type { ResolvedTheme } from '@/types';
import { useHeaderRefresh } from '@/hooks/useHeaderRefresh';
import { formatPercent } from '@/utils/format';
import type { QuotaStatsPool, QuotaStatsQuery } from '@/services/api/quotaStats';
import {
  getAuthFileStatusMessage,
  getTypeLabel,
  isProblemAuthFile,
} from '@/features/authFiles/constants';
import { QUOTA_PROVIDER_ACCENTS, QUOTA_TAB_ORDER } from '@/features/quota/constants';
import { QuotaProviderIcon } from '@/features/quota/components/QuotaProviderIcon';
import { useQuotaStats } from '@/features/quota/hooks/useQuotaStats';
import { classifyQuotaFiles } from '@/features/quota/logic';
import type { QuotaProviderType } from '@/features/quota/providers/types';
import { useDashboardOverview } from './hooks/useDashboardOverview';
import { Meter } from './components/Meter';
import { Sparkline } from './components/Sparkline';
import { ThroughputChart } from './components/ThroughputChart';
import { providerLabel, splitWindowMinutes, toneForSuccessRate } from './utils';
import styles from './dashboard.module.scss';

const DASH = '—';
const HOUR_MS = 3_600_000;

const isQuotaProvider = (value: string): value is QuotaProviderType =>
  (QUOTA_TAB_ORDER as readonly string[]).includes(value);

export function DashboardPage() {
  const { t, i18n } = useTranslation();
  const serverVersion = useAuthStore((state) => state.serverVersion);
  const resolvedTheme = useThemeStore((state) => state.resolvedTheme);
  const {
    connectionStatus,
    connected,
    config,
    counts,
    traffic,
    providers,
    credentials,
    authFiles,
    refresh,
  } = useDashboardOverview();

  const [now, setNow] = useState(() => Date.now());
  useHeaderRefresh(async () => {
    setNow(Date.now());
    await refresh();
  }, connected);

  const quotaQuery = useMemo<QuotaStatsQuery>(
    () => ({ from: now - 24 * HOUR_MS, to: now, rateHours: 3, usageMultiplier: 1 }),
    [now]
  );
  const quotaStats = useQuotaStats(quotaQuery, now);

  const quotaProviders = useMemo(() => {
    const present = new Set(classifyQuotaFiles(authFiles ?? []).map((entry) => entry.type));
    return QUOTA_TAB_ORDER.filter((provider) => present.has(provider));
  }, [authFiles]);

  const attention = useMemo(
    () =>
      (authFiles ?? []).filter(isProblemAuthFile).map((file) => ({
        key: file.name,
        name: file.email || file.name,
        provider: String(file.type ?? ''),
        message: getAuthFileStatusMessage(file) || t('dashboard.health_unavailable'),
      })),
    [authFiles, t]
  );

  const windowLabel = useMemo(() => {
    if (traffic.windowMinutes <= 0) return DASH;
    const { hours, minutes } = splitWindowMinutes(traffic.windowMinutes);
    if (hours === 0) return t('dashboard.window_m', { minutes });
    if (minutes === 0) return t('dashboard.window_h', { hours });
    return t('dashboard.window_hm', { hours, minutes });
  }, [traffic.windowMinutes, t]);

  const instant = (at: number) =>
    new Date(at).toLocaleString(i18n.language, {
      weekday: 'short',
      hour: 'numeric',
      minute: '2-digit',
    });

  const connectionLabel = t(
    connectionStatus === 'connected'
      ? 'common.connected'
      : connectionStatus === 'connecting'
        ? 'common.connecting'
        : 'common.disconnected'
  );
  const headerMeta = [
    serverVersion ? `v${serverVersion.trim().replace(/^[vV]+/, '')}` : null,
    connectionLabel,
    credentials
      ? t('dashboard.meta_accounts', {
          count: credentials.total,
          defaultValue: '{{count}} accounts',
        })
      : null,
    counts.models === null
      ? null
      : t('dashboard.meta_models', { count: counts.models, defaultValue: '{{count}} models' }),
  ].filter(Boolean);

  const routing = config?.raw?.routing as Record<string, unknown> | undefined;
  const yesNo = (value: boolean | undefined) =>
    !config ? DASH : value ? t('common.yes') : t('common.no');
  const strategy = config?.routingStrategy?.trim() ?? '';
  const strategyLabels: Record<string, string> = {
    'round-robin': t('dashboard.routing_round_robin', { defaultValue: 'Round robin' }),
    'weighted-round-robin': t('dashboard.routing_weighted', {
      defaultValue: 'Weighted round robin',
    }),
    'fill-first': t('dashboard.routing_fill_first', { defaultValue: 'Fill first' }),
  };
  const runtimeRows = [
    {
      label: t('dashboard.runtime_routing'),
      value: strategyLabels[strategy] ?? (strategy || DASH),
    },
    {
      label: t('dashboard.runtime_session_affinity', { defaultValue: 'Session affinity' }),
      value:
        routing?.['session-affinity'] === true
          ? String(routing['session-affinity-ttl'] ?? t('common.yes'))
          : t('common.no'),
    },
    { label: t('dashboard.runtime_retry'), value: String(config?.requestRetry ?? 0) },
    { label: t('dashboard.runtime_proxy'), value: config?.proxyUrl?.trim() || DASH },
    { label: t('dashboard.runtime_file_logging'), value: yesNo(config?.loggingToFile) },
    { label: t('dashboard.runtime_request_log'), value: yesNo(config?.requestLog) },
  ];

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <h1>{t('nav.dashboard')}</h1>
        <p>{headerMeta.join(' · ')}</p>
      </header>

      <section className={styles.section} aria-labelledby="dashboard-quota">
        <div className={styles.sectionHead}>
          <h2 id="dashboard-quota">{t('nav.quota_management')}</h2>
          <Link to="/quota">
            {t('dashboard.open_quota', { defaultValue: 'Open quota' })} <span aria-hidden>→</span>
          </Link>
        </div>
        {quotaProviders.length === 0 ? (
          <p className={styles.empty}>{t('dashboard.health_empty')}</p>
        ) : (
          <div className={styles.pools}>
            {quotaProviders.map((provider) => (
              <PoolTile
                key={provider}
                provider={provider}
                label={getTypeLabel(t, provider)}
                pool={quotaStats?.data?.pools.find((pool) => pool.provider === provider)}
                state={!quotaStats ? 'loading' : quotaStats.error ? 'unavailable' : 'ready'}
                now={now}
                instant={instant}
                resolvedTheme={resolvedTheme}
              />
            ))}
          </div>
        )}
      </section>

      <section className={`${styles.section} ${styles.card}`} aria-labelledby="dashboard-requests">
        <div className={styles.requestsHead}>
          <div>
            <h2 id="dashboard-requests">
              {t('dashboard.requests_title', { defaultValue: 'Requests' })}
            </h2>
            <span className={styles.muted}>
              {t('dashboard.requests_window', {
                window: windowLabel,
                defaultValue: 'Last {{window}}',
              })}
            </span>
          </div>
          <dl className={styles.figures}>
            <div>
              <dt>{t('dashboard.requests_total', { defaultValue: 'Total' })}</dt>
              <dd>{connected ? traffic.total.toLocaleString() : DASH}</dd>
            </div>
            <div>
              <dt>{t('dashboard.success_rate')}</dt>
              <dd data-tone={toneForSuccessRate(traffic.successRate)}>
                {traffic.successRate === null ? DASH : formatPercent(traffic.successRate)}
              </dd>
            </div>
            <div>
              <dt>{t('stats.failure')}</dt>
              <dd>{traffic.totalFailure.toLocaleString()}</dd>
            </div>
          </dl>
        </div>
        <ThroughputChart traffic={traffic} />
        {providers.length > 0 && (
          <ul className={styles.providerRows}>
            {providers.map((provider) => {
              const label = providerLabel(provider.id, t('dashboard.provider_unknown'));
              return (
                <li key={provider.id}>
                  <span className={styles.providerName}>
                    {isQuotaProvider(provider.id) && (
                      <QuotaProviderIcon provider={provider.id} resolvedTheme={resolvedTheme} />
                    )}
                    {label}
                    <span className={styles.muted}>
                      {t('dashboard.fleet_credentials', { value: provider.credentials })}
                    </span>
                  </span>
                  <Sparkline
                    points={provider.buckets.map((bucket) => bucket.success + bucket.failed)}
                    ariaLabel={t('dashboard.fleet_spark_label', { provider: label })}
                    className={styles.providerSpark}
                  />
                  <span className={styles.providerTotal}>{provider.total.toLocaleString()}</span>
                  <span className={styles.providerRate}>
                    {provider.successRate === null ? DASH : formatPercent(provider.successRate)}
                    <Meter value={provider.successRate} ariaLabel={t('dashboard.success_rate')} />
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <div className={styles.columns}>
        <section className={styles.card} aria-labelledby="dashboard-attention">
          <div className={styles.cardHead}>
            <h2 id="dashboard-attention">
              {t('dashboard.attention_title', { defaultValue: 'Needs attention' })}
            </h2>
            <Link to="/auth-files">
              {t('nav.auth_files')} <span aria-hidden>→</span>
            </Link>
          </div>
          {!credentials ? (
            <p className={styles.empty}>{DASH}</p>
          ) : attention.length === 0 ? (
            <p className={styles.healthy}>
              <span className={styles.healthyDot} aria-hidden />
              {t('dashboard.attention_none', {
                count: credentials.total - credentials.disabled,
                defaultValue: 'All {{count}} active accounts are healthy',
              })}
              {credentials.disabled > 0 && (
                <span className={styles.muted}>
                  {' · '}
                  {t('dashboard.attention_disabled', {
                    count: credentials.disabled,
                    defaultValue: '{{count}} disabled',
                  })}
                </span>
              )}
            </p>
          ) : (
            <ul className={styles.attention}>
              {attention.map((item) => (
                <li key={item.key}>
                  <span className={styles.providerName}>
                    {isQuotaProvider(item.provider) && (
                      <QuotaProviderIcon provider={item.provider} resolvedTheme={resolvedTheme} />
                    )}
                    {item.name}
                  </span>
                  <span className={styles.attentionMessage}>{item.message}</span>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className={styles.card} aria-labelledby="dashboard-runtime">
          <div className={styles.cardHead}>
            <h2 id="dashboard-runtime">{t('dashboard.runtime_title')}</h2>
            <Link to="/config">
              {t('nav.config_management')} <span aria-hidden>→</span>
            </Link>
          </div>
          <dl className={styles.runtime}>
            {runtimeRows.map((row) => (
              <div key={row.label}>
                <dt>{row.label}</dt>
                <dd>{row.value}</dd>
              </div>
            ))}
          </dl>
        </section>
      </div>
    </div>
  );
}

function PoolTile({
  provider,
  label,
  pool,
  state,
  now,
  instant,
  resolvedTheme,
}: {
  provider: QuotaProviderType;
  label: string;
  pool: QuotaStatsPool | undefined;
  state: 'loading' | 'unavailable' | 'ready';
  now: number;
  instant: (at: number) => string;
  resolvedTheme: ResolvedTheme;
}) {
  const { t } = useTranslation();
  const percent = pool && pool.capacity > 0 ? (pool.remaining / pool.capacity) * 100 : null;
  const nextReset = pool?.accounts
    .map((account) => account.resetAt)
    .filter((at): at is number => at !== null && at > now)
    .sort((a, b) => a - b)[0];
  const firstEmpty = pool?.projection.available ? pool.projection.emptyAccounts[0] : undefined;
  const status =
    state === 'loading'
      ? t('quota_management.pool_loading', { defaultValue: 'Loading quota history…' })
      : state === 'unavailable'
        ? t('dashboard.quota_unavailable', { defaultValue: 'History unavailable' })
        : !pool
          ? t('dashboard.quota_no_samples', { defaultValue: 'No samples yet' })
          : null;

  return (
    <Link
      to="/quota"
      className={styles.pool}
      style={{ '--pool-accent': QUOTA_PROVIDER_ACCENTS[provider] } as CSSProperties}
    >
      <span className={styles.providerName}>
        <QuotaProviderIcon provider={provider} resolvedTheme={resolvedTheme} />
        {label}
        {pool && (
          <span className={styles.muted}>
            {pool.accountCount === 1
              ? t('dashboard.pool_account', { defaultValue: '1 account' })
              : t('dashboard.pool_accounts', {
                  count: pool.accountCount,
                  defaultValue: '{{count}} accounts',
                })}
          </span>
        )}
      </span>
      {status ? (
        <span className={styles.poolStatus}>{status}</span>
      ) : (
        <>
          <span className={styles.poolFigure}>
            <strong>{percent === null ? DASH : `${Math.round(percent)}%`}</strong>
            {t('dashboard.pool_left', { defaultValue: 'left' })}
          </span>
          <span className={styles.poolBar} aria-hidden>
            <span style={{ width: `${Math.max(0, Math.min(100, percent ?? 0))}%` }} />
          </span>
          <dl className={styles.poolFacts}>
            <div>
              <dt>{t('dashboard.pool_burn', { defaultValue: 'Burn' })}</dt>
              <dd>{pool?.drain == null ? DASH : `${Math.round(pool.drain * 10) / 10}%/h`}</dd>
            </div>
            <div>
              <dt>{t('dashboard.pool_reset', { defaultValue: 'Next reset' })}</dt>
              <dd>{nextReset ? instant(nextReset) : DASH}</dd>
            </div>
            <div>
              <dt>{t('dashboard.pool_first_empty', { defaultValue: 'First empty' })}</dt>
              <dd>
                {!pool?.projection.available
                  ? DASH
                  : firstEmpty
                    ? instant(firstEmpty.at)
                    : t('dashboard.pool_not_soon', { defaultValue: 'Not in 14 days' })}
              </dd>
            </div>
          </dl>
        </>
      )}
    </Link>
  );
}
