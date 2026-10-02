import { useEffect, useRef } from 'react';
import { useQuotaStore } from '@/stores/useQuotaStore';
import { getQuotaCacheKey } from '@/utils/quota/identity';
import type { QuotaFileEntry } from '../logic';
import { QUOTA_ADAPTERS, getQuotaMap } from '../providers';
import type { QuotaProviderType } from '../providers/types';

/**
 * Load quota for visible credentials without a click.
 *
 * Each credential is attempted once per visit, per session, per file
 * generation, so a re-render never re-queries an upstream provider. No
 * polling. Devin always auto-loads; the rest follow the page preference.
 */
export function useQuotaAutoLoad(
  entries: QuotaFileEntry[],
  disabled: boolean,
  loadQuota: (targets: QuotaFileEntry[]) => Promise<void>,
  providers: ReadonlySet<QuotaProviderType>
) {
  const attempted = useRef(new Set<string>());
  const session = useQuotaStore((state) => state.cacheGeneration);
  const fileGenerations = useQuotaStore((state) => state.fileGenerations);

  useEffect(() => {
    if (disabled) return;
    const targets = entries.filter(({ type, file }) => {
      if (!providers.has(type)) return false;
      const key = JSON.stringify([
        session,
        fileGenerations[file.name] ?? 0,
        file.name,
        file.authIndex,
      ]);
      if (attempted.current.has(key)) return false;
      attempted.current.add(key);
      // An explicit refresh already started in this effect cycle counts too.
      const current = getQuotaMap(QUOTA_ADAPTERS[type])[getQuotaCacheKey(file)];
      return current?.status !== 'loading' && current?.status !== 'success';
    });
    if (targets.length > 0) void loadQuota(targets);
  }, [disabled, entries, fileGenerations, loadQuota, providers, session]);
}
