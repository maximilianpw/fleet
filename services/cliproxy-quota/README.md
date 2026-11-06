# cliproxy-quota

Quota-only view of CLIProxyAPI's provider credentials. Two programs, both
TypeScript run directly by Bun with no runtime dependencies:

- `cliproxy-quota-server`: loopback HTTP endpoint on the CLIProxyAPI host.
- `cliproxyapi-util quota [--json]`: report for any host, read over HTTP.

Moved here from `pi-config` (`cli/cliproxyapi-quota-server.ts`,
`cli/cliproxyapi-util.ts`, and the readers in
`extensions/cliproxyapi/client.ts`) by stage 4 of
[the monorepo plan](../../docs/monorepo-migration-plan.md).

## HTTP contract

`GET /quota/v1/{claude,codex,xai}`:

| Status | Meaning |
| --- | --- |
| 200 | JSON below, `cache-control: no-store` |
| 404 | Unknown path or provider, non-GET method, or no enabled credential |
| 503 | No account has a usable answer: each upstream request failed or timed out (8 s), or is backing off with no earlier answer |

```json
{
  "provider": "codex",
  "windows": [{ "label": "7d", "usedPercent": 42, "resetAtMs": 1788454137000 }],
  "readyAccounts": 1,
  "totalAccounts": 2,
  "fetchedAtMs": 1790000000000
}
```

`windows` describes the least-used ready account, or the least-used account
if none is ready. `fetchedAtMs` is when that account's answer was fetched,
which may be earlier than the request. Credential files are re-read on every
request. Tokens never leave the server.

## Upstream requests

Provider usage endpoints are rate-limited per account, and that limit is
shared with the account's own clients (for example Claude Code's `/usage`).
So `/quota/v1` and `/metrics` share one per-account cache:

- Each account's provider API is called at most once per 60 seconds, and
  concurrent callers wait on the same request. A caller that disconnects
  stops waiting; the request still completes and fills the cache.
- After a failure the account is not called again until a backoff passes:
  30 s, doubling to 15 min, or the provider's `Retry-After` if longer
  (capped at 1 h). A changed access token in the credential file ends the
  backoff. Each failure is logged to stderr with its HTTP status.
- During a backoff the last successful answer is served until it is 30
  minutes old.

## Metrics

`GET /metrics` returns per-account gauges in Prometheus text format, labelled
by `provider` and `account` (the credential filename without `.json`):

| Metric | Labels | Meaning |
| --- | --- | --- |
| `cliproxy_quota_up` | provider, account | 1 if the account's latest quota request succeeded |
| `cliproxy_quota_ready` | provider, account | 1 if the provider accepts requests for it |
| `cliproxy_quota_fetched_timestamp_seconds` | provider, account | When the reported answer was fetched, Unix seconds |
| `cliproxy_quota_used_percent` | + window | Percentage of the window used |
| `cliproxy_quota_reset_timestamp_seconds` | + window | Window reset, Unix seconds |

Scrapes use the cache above, so they never add upstream requests beyond one
per account per minute. An account whose latest request failed reports
`cliproxy_quota_up 0`; its ready and window series continue from the last
answer while it is within the stale limit, and are omitted after that. An
unreadable credential directory returns 503.

## Quota history

With `--prometheus-url` and `--management-url` (both loopback), the server
also serves the prox UI's quota history contract from the Prometheus series
above, in place of the ClickHouse backend:

- `GET /observability/quota/history`
- `GET /observability/quota/pools`

Parameters, response shapes and status codes follow CLIProxyAPI's
`docs/quota-statistics.md`: `from`/`to` in epoch milliseconds (default the
last 24 hours, at most 31 days), `provider`, `account`, `rateHours` 1/3/6 and
`usageMultiplier` 0/0.5/1/2. Each account contributes its longest recognized
window (7d over 5h). Accounts are identified by their CLIProxyAPI
`auth_index`, read from `GET /v8/management/credentials`.

Requests forward their `Authorization` header to that management endpoint,
so the management key is checked by CLIProxyAPI itself and its 401, 403 or
404 is returned unchanged. Route `/v8/management/observability/quota/` here
from the reverse proxy that fronts the management API.

The `/quota/v1` and `/metrics` endpoints have no authentication. It only binds loopback addresses. Remote
clients go through a reverse proxy that checks a bearer token and strips it
before forwarding.

## Server

```sh
cliproxy-quota-server --credential-dir /home/alice/.cli-proxy-api \
  [--listen-address 127.0.0.1] [--port 8318]
```

On NixOS, use the module instead:

```nix
{
  imports = [inputs.fleet.nixosModules.cliproxy-quota];
  services.cliproxyapi-quota = {
    enable = true;
    user = "alice";
    credentialDirectory = "/home/alice/.cli-proxy-api";
  };
}
```

It defines `systemd.services.cliproxyapi-quota` as a sandboxed unit with a
read-only home.

## Client settings

`cliproxyapi-util` reads the environment, then
`~/.config/cliproxyapi/client.json`:

| Environment | client.json | Meaning |
| --- | --- | --- |
| `CLIPROXYAPI_QUOTA_URL` | `quotaUrl` | Endpoint base ending in `/quota/v1` |
| `CLIPROXYAPI_ROOT_URL` | `rootUrl` | Fallback `${rootUrl}/quota/v1`, public URLs only |
| `CLIPROXYAPI_API_KEY` | | Bearer token |
| `CLIPROXYAPI_API_KEY_FILE` | `apiKeyFile` | File holding the bearer token |

A loopback quota URL is the server host's direct access and needs no key.
On a remote host, a 404 means no credential on the server and is reported as
`unavailable`.

## Checks

```sh
bun install --frozen-lockfile && bun run check
nix build path:$PWD#checks.x86_64-linux.cliproxy-quota path:$PWD#checks.x86_64-linux.cliproxy-quota-nixos --no-link
```

The NixOS VM test uses a fictional user and credential file. The VM has no
internet, so it never reaches real provider APIs.
