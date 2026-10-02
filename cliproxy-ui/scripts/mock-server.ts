// Minimal fake CLIProxyAPI v8 management backend for UI development.
// Run: bun scripts/mock-server.ts  (listens on :8317, any management key accepted)

const now = Date.now();
const iso = (hours: number) => new Date(now + hours * 3600_000).toISOString();
const unix = (hours: number) => Math.floor((now + hours * 3600_000) / 1000);

type Cred = { name: string; type: string; email: string; claude?: [number, number, number, number, number]; codex?: [number, number] };

// claude: [fable%, fableResetH, fiveHour%, fiveHourResetH(0 = none), sevenDay%]
const creds: Cred[] = [
  { name: 'claude-team@lumen.example.json', type: 'claude', email: 'team@lumen.example', claude: [42, 26, 0, 0, 21] },
  { name: 'claude-tools@pixel.example.json', type: 'claude', email: 'tools@pixel.example', claude: [0, 98, 0, 0, 0] },
  { name: 'claude-test@tango.example.json', type: 'claude', email: 'test@tango.example', claude: [0, 112, 0, 0, 0] },
  { name: 'claude-trial@tempo.example.json', type: 'claude', email: 'trial@tempo.example', claude: [49, 34, 1, 3, 25] },
  { name: 'claude-ops@north.example.json', type: 'claude', email: 'ops@north.example', claude: [82, 18, 64, 2, 71] },
  { name: 'codex-main@lumen.example.json', type: 'codex', email: 'main@lumen.example', codex: [83, 50] },
  { name: 'codex-alt@pixel.example.json', type: 'codex', email: 'alt@pixel.example', codex: [100, 50] },
  { name: 'codex-ci@tango.example.json', type: 'codex', email: 'ci@tango.example', codex: [64, 120] },
  { name: 'xai-grok@lumen.example.json', type: 'xai', email: 'grok@lumen.example' },
  { name: 'kimi-coder@lumen.example.json', type: 'kimi', email: 'coder@lumen.example' },
];

const files = creds.map((c, i) => ({
  name: c.name,
  type: c.type,
  provider: c.type,
  email: c.email,
  auth_index: `idx-${i}`,
  size: 1024,
  status: 'active',
  disabled: false,
  modified: now - i * 86400_000,
  success: 120 + i * 37,
  failed: i % 3,
}));

const byIndex = (idx: string) => creds[Number(idx.replace('idx-', ''))];

function upstream(url: string, authIndex: string): { status: number; body: unknown } {
  const c = byIndex(authIndex);
  if (!c) return { status: 404, body: { error: 'unknown auth' } };
  if (url.includes('api.anthropic.com/api/oauth/profile')) {
    return { status: 200, body: { account: { has_claude_max: true, email: c.email }, organization: { organization_type: 'claude_max' } } };
  }
  if (url.includes('api.anthropic.com/api/oauth/usage') && c.claude) {
    const [fable, fableH, five, fiveH, seven] = c.claude;
    return {
      status: 200,
      body: {
        five_hour: { utilization: five, resets_at: fiveH ? iso(fiveH) : null },
        seven_day: { utilization: seven, resets_at: iso(fableH) },
        limits: [
          { kind: 'weekly_scoped', group: 'weekly', percent: fable, resets_at: iso(fableH), is_active: true, scope: { model: { id: null, display_name: 'Fable' } } },
        ],
      },
    };
  }
  if (url.includes('chatgpt.com/backend-api/wham/usage') && c.codex) {
    const [used, resetH] = c.codex;
    return {
      status: 200,
      body: {
        plan_type: 'pro',
        rate_limit: {
          allowed: true,
          limit_reached: used >= 100,
          primary_window: { used_percent: used, limit_window_seconds: 604800, reset_after_seconds: resetH * 3600, reset_at: unix(resetH) },
          secondary_window: null,
        },
        code_review_rate_limit: null,
        additional_rate_limits: [],
        rate_limit_reset_credits: { available_count: 0, applicable_available_count: 0 },
      },
    };
  }
  if (url.includes('kimi')) {
    return {
      status: 200,
      body: {
        usage: { limit: '100', used: '0', remaining: '100', resetTime: iso(122) },
        limits: [{ detail: { limit: '100', used: '0', remaining: '100', resetTime: iso(3) }, window: { duration: 300, timeUnit: 'TIME_UNIT_MINUTE' } }],
        usages: {},
      },
    };
  }
  return { status: 404, body: { error: { message: 'not mocked' } } };
}

const cors = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Headers': '*',
  'Access-Control-Allow-Methods': 'GET,POST,PUT,PATCH,DELETE,OPTIONS',
  'Access-Control-Expose-Headers': '*',
  'X-CPA-Version': 'v8.4.0-mock',
  'X-CPA-Build-Date': new Date(now).toISOString(),
};
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { ...cors, 'Content-Type': 'application/json' } });

Bun.serve({
  port: 8317,
  async fetch(req) {
    const url = new URL(req.url);
    if (req.method === 'OPTIONS') return new Response(null, { headers: cors });
    const path = url.pathname.replace(/^\/v8\/management/, '');
    if (path === '/config') return json({ debug: false, 'request-retry': 3, access: { 'api-keys': ['sk-demo'] } });
    if (path === '/credentials' && req.method === 'GET') return json({ files, total: files.length });
    if (path === '/credentials/download') {
      const name = url.searchParams.get('name') ?? '';
      const c = creds.find((x) => x.name === name);
      return new Response(JSON.stringify({ type: c?.type, email: c?.email }), { headers: cors });
    }
    if (path === '/requests/api-call') {
      const payload = (await req.json()) as { url: string; authIndex?: string; auth_index?: string };
      const { status, body } = upstream(payload.url, payload.authIndex ?? payload.auth_index ?? '');
      return json({ status_code: status, header: {}, body: JSON.stringify(body) });
    }
    if (path.startsWith('/observability/logs')) return json({ lines: [], 'line-count': 0, 'latest-timestamp': 0 });
    if (path === '/plugins') return json({ plugins: [] });
    return json({});
  },
});
console.log('mock CPA on http://localhost:8317');
