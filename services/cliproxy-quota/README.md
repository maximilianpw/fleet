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
| 503 | Every account's upstream request failed or timed out (8 s) |

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
if none is ready. Results are cached per provider for 15 seconds. Credential
files are re-read on every uncached request. Tokens never leave the server.

The endpoint has no authentication. It only binds loopback addresses. Remote
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
