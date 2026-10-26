# prox backend and ClickHouse on NixOS

Fleet exports `packages.x86_64-linux.cliproxyapi` and
`nixosModules.cliproxyapi`. The package builds the customized
[CLIProxyAPI quota-statistics branch](https://github.com/maximilianpw/CLIProxyAPI/pull/1)
at `c050425e8dcc70654bef4745ccf62eed699f39da`, including its schema and
[API documentation](https://github.com/maximilianpw/CLIProxyAPI/blob/c050425e8dcc70654bef4745ccf62eed699f39da/docs/quota-statistics.md).
It does not follow a moving branch or require that PR to be merged. Go 1.26.10
is built with a pinned source hash; Fleet's nixpkgs and Rust pins stay unchanged.

This is reusable wiring, **not a host activation**. Host selection, SOPS/agenix
secret declarations, domains, Nginx ingress and backups belong in the consumer
Nix configuration. No production credentials or inventory belong in Fleet.

## Adopt without moving auth files

Replace the consumer's hand-written `systemd.services.cliproxyapi` definition
with this module; do not run a second backend on the same port or merge two
different `ExecStart`/`MANAGEMENT_STATIC_PATH` definitions. Keep the current
service user, working directory, runtime config path, management key, API keys,
routing and upstream configuration. **Use the existing auth directory exactly**:
do not copy it into the store, rename it, recreate it or recursively chown it.
File names influence credential identities; changing them can disconnect history.

Example using fictional paths; substitute the consumer's existing values:

```nix
{ config, inputs, ... }: {
  imports = [ inputs.fleet.nixosModules.cliproxyapi ];
  services.cliproxyapi = {
    enable = true;
    user = "alice";
    workingDirectory = "/home/alice";
    credentialDirectory = "/home/alice/.cli-proxy-api";
    configFile = config.sops.templates."cliproxyapi.conf".path;
    environmentFile = config.sops.templates."cliproxyapi-quota.env".path;
    localClickHouse = {
      enable = true;
      environmentFile = config.sops.templates."cliproxyapi-clickhouse.env".path;
    };
  };
}
```

The existing YAML must keep `oauth.auth-dir` equal to `credentialDirectory` as
an absolute path (legacy `auth-dir` is also accepted). Startup fails if the
directory is missing or the setting differs. No automatic credential migration
runs. The backend can still refresh and upload credentials in that directory;
the rest of the home directory is read-only. Runtime logs/state go to
`/var/lib/cliproxyapi`, **not** a replacement auth store.

The backend's existing Codex persistence code may atomically reserialize JSON
and tighten file permissions to `0600` during startup. Preservation means the
same path, credential identity and JSON data, not identical bytes or inode.

Add `management.disable-auto-update-panel: true` to the existing v8 SOPS YAML:
the UI comes from the pinned Fleet package, not upstream's automatic downloader.
The module sets `MANAGEMENT_STATIC_PATH` to that package. Keep the existing
Nginx `/management.html` alias to Fleet's UI and `/v8/management/` proxy route;
the management bearer header must reach the backend unchanged. No extra public
ClickHouse route, firewall port or browser database credential is needed.

Leave `services.cliproxyapi-quota` enabled with its **same** credentialDirectory.
Its `/quota/v1/{claude,codex,xai}` endpoint is independent and unchanged. The
backend and this read-only sidecar intentionally share the existing auth files.

## Runtime secrets and initialization

Use the consumer's existing encrypted secret mechanism. Two root-readable,
mode `0400` systemd EnvironmentFiles are needed for local ClickHouse:

* Backend file: `CLICKHOUSE_QUOTA_PASSWORD=<runtime-password>`.
* ClickHouse file: the same `CLICKHOUSE_QUOTA_PASSWORD=<runtime-password>` plus
  a **different** `CLICKHOUSE_QUOTA_ADMIN_PASSWORD=<admin-password>`.

Those are placeholders, not values to commit. With sops-nix, render the two
files using `config.sops.placeholder."<secret-name>"`; keep the same runtime
secret reference in both templates. Systemd loads them before dropping user
privileges, so neither file needs to be readable by the proxy user. Prefer
generated hex secrets, which avoid both systemd EnvironmentFile escaping and
ClickHouse's XML environment-substitution escaping. Other formats require
validation against both parsers; shell quoting is not sufficient.
Set template `restartUnits` to `cliproxyapi.service` for the backend file and
`clickhouse.service` for the ClickHouse file. Coordinate rotations of both files.
Do not place the admin password in the backend's file. Keep the existing
management key in its existing runtime config; no new management key is needed.

Local mode binds ClickHouse to loopback, uses database `cliproxy_quota`, and
gives user `prox_quota` only `SELECT`/`INSERT` on `quota_samples`. The `default`
administrative user also requires a password and is loopback-only. Passwords
are read from the environment and hidden from ClickHouse's preprocessed config.
The backend receives the local HTTP endpoint, database and runtime username
automatically. Do not override those in its EnvironmentFile in local mode.

The ordered `cliproxyapi-quota-schema` oneshot creates the database and applies
the pinned `CREATE TABLE IF NOT EXISTS` before starting the backend. Enabling
the module on a host therefore authorizes this initialization **at activation**;
review it first. It does not drop data or alter an existing table. An existing
incompatible schema needs an operator-reviewed migration, not an assumed upgrade.
ClickHouse data persists in `/var/lib/clickhouse`; back it up independently of
OAuth credentials. The schema retains observations for 90 days (TTL cleanup is
asynchronous).

For an existing/external ClickHouse, leave `localClickHouse.enable = false`,
supply `CLICKHOUSE_QUOTA_ENDPOINT`, `CLICKHOUSE_QUOTA_DATABASE`,
`CLICKHOUSE_QUOTA_USERNAME` and `CLICKHOUSE_QUOTA_PASSWORD` in the backend
EnvironmentFile, and apply `$out/share/cliproxyapi/quota-schema.sql` separately
with administrative credentials. Use HTTPS off-host. Local mode is intended
for a dedicated ClickHouse instance; do not overlay it on another workload's
users/configuration without reviewing the merged configuration.

## What will populate the charts

The UI calls authenticated `/v8/management/observability/quota/history` and
`/pools`. ClickHouse is the source of observations; the backend computes rates
and forecasts. An empty configured database returns 200 with empty results,
not fabricated history. Disabled storage returns 503; failures return 502.

**The pinned backend automatically ingests successful v8 plugin quota fetches
only. Native-provider `/requests/api-call` fetches and the separate quota
sidecar do not insert samples.** There is no collector daemon in this module.
For native providers or unattended collection, an operator must provide a
collector that inserts the canonical samples documented upstream, using the
credential `auth_index` from the management API. Do not invent observations,
backfill failed fetches as zero, or infer quota from token usage.

Burn/drain need at least two usable observations spanning five minutes in the
selected rate window. Forecasts additionally require fresh observations and
known future resets/full-refill periods for every observed account. Missing
rates/projections remain unavailable. Capacity covers observed accounts only,
not every credential configured on the host.

## Rollout and verification

1. Pin the desired Fleet revision in the consumer configuration; keep a rollback
   generation and back up the existing auth directory and ClickHouse data.
2. Replace only the backend unit/package wiring, preserve all runtime config
   values above, render the two secret files, and build the consumer system.
   Review the activation diff before approving a live switch.
3. After an authorized switch, inspect `clickhouse`, `cliproxyapi-quota-schema`,
   `cliproxyapi` and `cliproxyapi-quota` units. Check the credential list and
   unchanged `auth_index` values through the existing prox domain. Verify the
   `/v8/management/observability/quota/pools` response using the existing
   management login; empty 200 is healthy storage, not evidence of collection.
4. Generate real observations through supported ingestion or an approved
   collector. Confirm nonempty history and rates after sufficient elapsed time.
   Keep database ports private. Rollback changes the backend binary/config,
   not the credential location, and must not delete ClickHouse data.

Repository checks:

```sh
nix build path:$PWD#cliproxyapi path:$PWD#checks.x86_64-linux.cliproxyapi-nixos --no-link
alejandra --check flake.nix nix
```

The VM test uses the packaged backend, real ClickHouse and Nginx with disposable
credentials/data. It checks authenticated empty/populated reads, exact rates,
schema initialization/restart, least privilege, existing auth identity/content,
credential upload to the same directory, sidecar coexistence, pinned HTML and
failure on a mismatched auth path. It does not contact provider APIs or activate
the consumer's host.
