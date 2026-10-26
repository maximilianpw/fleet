const MARK = (
  <>
    <rect width="32" height="32" rx="9" fill="#1c1c1c" />
    <path
      d="M7 8h4l8 8"
      stroke="#edac65"
      strokeWidth="3.5"
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
    <path
      d="M7 24h4l8-8"
      stroke="#83b0f7"
      strokeWidth="3.5"
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
    <path d="M7 16h18" stroke="#f1f1ef" strokeWidth="3.5" strokeLinecap="round" />
  </>
);

export const PROX_LOGO_DATA_URL = `data:image/svg+xml,${encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="9" fill="#1c1c1c"/><path d="M7 8h4l8 8" stroke="#edac65" stroke-width="3.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M7 24h4l8-8" stroke="#83b0f7" stroke-width="3.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M7 16h18" stroke="#f1f1ef" stroke-width="3.5" stroke-linecap="round"/></svg>'
)}`;

export function ProxLogo({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 32 32" role="img" aria-label="prox">
      {MARK}
    </svg>
  );
}
