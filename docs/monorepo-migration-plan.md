# Fleet monorepo migration plan

Status: stages 1–4 done. Kim runs the packaged quota service, verified in
local and remote client mode; Pi reads it over HTTP and pi-config no longer
contains the implementation. Stage 5 (update workflow documentation) not
started.

Make Fleet the monorepo for operational software, with nix-config selecting
versions and configuring each machine. UI updates will use pinned Nix packages
and roll back with the system.

The initial scope is the Fleet CLI, CLIProxy UI, and shared CLIProxy quota
service. Complete stages 1–3 first, then migrate quota separately.

## 1. Establish the monorepo layout

Reorganize the repository without changing behavior:

```text
fleet/
  Cargo.toml                 # Rust workspace
  Cargo.lock
  cli/                       # Existing Fleet CLI
  apps/cliproxy-ui/
  services/cliproxy-quota/
  nix/packages/
  nix/modules/
  docs/
  .github/workflows/
```

Move the Rust CLI and UI using Git moves. Preserve the `fleet` and
`fleet-tunnel-runner` executables, command behavior, configuration schema, and
launchd labels.

Keep root Cargo commands working through a workspace. Preserve existing flake
outputs such as `packages.<system>.fleet` and `homeManagerModules.default`.

Update package paths, CI working directories, documentation, and repository
guidance to match the layout. Retain the CLI's existing Rust toolchain pin.

Acceptance: existing Rust, Home Manager, and UI tests pass after the moves.

## 2. Package the UI with Nix

Add `packages.<system>.cliproxy-ui`, producing:

```text
$out/share/cliproxy-ui/management.html
```

Build from committed source and the Bun lockfile. Pin the build tools and fetch
dependencies through Nix. Compilation must work without an existing
`node_modules` directory or network access during the build phase.

Move UI verification into root GitHub Actions jobs. Its current workflows are
nested under `cliproxy-ui/.github/`, so GitHub does not discover them as
repository workflows.

Keep package source inputs scoped so editing the UI does not unnecessarily
rebuild the Rust CLI. Preserve the UI's single-file output, hash routing, and
upstream license. Supply build provenance explicitly rather than deriving it
from unrelated CLI tags.

Acceptance: build the package, verify the single-file output, and exercise
login, providers, models, and quota against a disposable backend.

## 3. Consume the UI package from nix-config

Update the pinned Fleet revision and make Nginx reference the package:

```nix
alias =
  "${inputs.fleet.packages.${pkgs.stdenv.hostPlatform.system}.cliproxy-ui}"
  + "/share/cliproxy-ui/management.html";
```

Keep the existing domain, API routes, backend, credentials, and cache policy.

Update the ingress regression test. Remove the copied
`assets/cliproxy-ui/management.html` and its manual provenance record once the
package supplies the artifact and provenance.

At planning time, the Fleet input in nix-config pins an older commit than the
working checkouts. Review the complete revision change before updating it.
Publish the approved source revision before pinning it in a consumer that must
fetch it remotely.

Acceptance: Nix evaluation, lint, ingress tests, and package builds pass. After
the operator rebuilds, compare the served HTML with the package output.

Rollback: the preceding Nix generation retains the previously deployed UI.

## 4. Extract the quota service from pi-config

Moving `cli/cliproxyapi-quota-server.ts` alone is insufficient. It imports
`extensions/cliproxyapi/client.ts`, which also reads provider credentials and
parses quota responses. The local Pi client currently uses those readers
directly, while remote clients use the quota HTTP endpoint.

Move the server, provider quota readers, parsers, caching, and their tests into
Fleet. Package the service and expose a reusable NixOS module with explicit
settings for its user, credential directory, and loopback listener.

Keep Pi-specific display and extension behavior in pi-config. Make Pi's quota
adapter consume the quota HTTP interface, with explicitly configured local
access on the server host and authenticated public access on other machines.
Treat that client change as a separate step from extracting the implementation.

Preserve `/quota/v1/{claude,codex,xai}` and its response format. Deploy the
packaged service before removing the old implementation. Migrate
`cliproxyapi-util` alongside its dependencies. Keep existing service names and
runtime credential locations during the transition.

Acceptance: fixture tests cover parsing, failures, timeouts, caching, and the
HTTP contract. Verify both local and remote client modes. The installed
service must no longer depend on a mutable pi-config checkout.

Rollback: retain the previous service and client revisions until the packaged
service and both client modes have been verified. No credential migration is
part of this extraction.

## 5. Document and verify updates

The normal update workflow becomes:

1. Change Fleet source.
2. Run checks and build packages.
3. Publish an approved Fleet revision.
4. Update the Fleet pin in nix-config.
5. Build and review the consumer configuration.
6. Rebuild the target machine.

Fleet owns reusable software, packages, and modules. Keep machine inventory,
domains, enabled services, firewall rules, storage, and SOPS references in
nix-config. Keep provider credentials as runtime data. Editor configuration
and Pi-specific extensions remain in their existing repositories.

Publishing and activation are separate steps requiring authorization. The
migration is complete when the UI and quota service come from pinned Fleet
packages, consumer checks pass, and updates no longer require manually copying
HTML or running service code from a mutable checkout.
