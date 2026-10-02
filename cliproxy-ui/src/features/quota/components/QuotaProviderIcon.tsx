import { useTranslation } from 'react-i18next';
import type { ResolvedTheme } from '@/types';
import {
  getAuthFileIcon,
  getThemeSurfaceIconBackground,
  getTypeLabel,
  isThemeSurfaceIconProvider,
} from '@/features/authFiles/constants';
import type { QuotaProviderType } from '../providers/types';

export type QuotaProviderIconProps = {
  provider: QuotaProviderType;
  resolvedTheme: ResolvedTheme;
  className?: string;
  size?: number;
};

/** Provider brand mark for the ledger and summary strip; falls back to the label's initial. */
export function QuotaProviderIcon({
  provider,
  resolvedTheme,
  className,
  size = 16,
}: QuotaProviderIconProps) {
  const { t } = useTranslation();
  const iconSrc = getAuthFileIcon(provider, resolvedTheme);
  const label = getTypeLabel(t, provider);

  return (
    <span
      className={className}
      title={label}
      style={
        isThemeSurfaceIconProvider(provider)
          ? { background: getThemeSurfaceIconBackground(resolvedTheme), borderRadius: 5 }
          : undefined
      }
    >
      {iconSrc ? (
        <img src={iconSrc} alt="" width={size} height={size} style={{ display: 'block' }} />
      ) : (
        <span aria-hidden="true">{label.slice(0, 1).toUpperCase()}</span>
      )}
    </span>
  );
}
