# Fleet

Monorepo for operational software: the Rust CLI with an optional Home Manager
module, plus the CLIProxy management UI. OpenSSH, tmux, and launchd stay the
CLI backends. Personal inventory, trust, and machine configuration stay in the
consumer Nix config. Staged migration status:
[docs/monorepo-migration-plan.md](docs/monorepo-migration-plan.md).

Read [docs/configuration.md](docs/configuration.md) before changing the TOML
schema, lookup order, or `programs.fleet.settings`. Read
[docs/compatibility.md](docs/compatibility.md) before changing command
dispatch, SSH argv, doctor, or launchd labels. Read
[docs/migration-baseline.md](docs/migration-baseline.md) before claiming
parity with the Bash sources.

## Safety

Use fictional hosts, temporary HOME/config/state, fake executables on a test
PATH, or child processes this task created. Disposable Nix and Cargo checks
are in bounds.

Production executable selection must not depend on fixture-only environment
variables. Configured launchd behavior is tested with a fake backend on
Linux. That is not a claim that real launchd exists there.

Public examples and tests use fictional hosts and temporary paths. Personal
inventory, keys, and absolute developer paths stay out of this repository.
Fleet's own licensing stays unset; there is no root LICENSE file.
`apps/cliproxy-ui/LICENSE` is the UI's upstream MIT license. Keep it.

## Commands

From the repo root. Done means exit 0.

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Nix, on Linux:

```sh
nix build path:$PWD#fleet path:$PWD#checks.x86_64-linux.fleet path:$PWD#checks.x86_64-linux.home-manager --no-link
alejandra --check flake.nix nix
```

UI, from the repo root:

```sh
nix build path:$PWD#cliproxy-ui path:$PWD#checks.x86_64-linux.cliproxy-ui --no-link
```

Or `bun install --frozen-lockfile && bun run verify` in `apps/cliproxy-ui`.
After changing `bun.lock`, set both hashes in `nix/packages/cliproxy-ui.nix`
to `lib.fakeHash` and rebuild to get the new values. The `--os`/`--cpu`
overrides let both be computed on Linux.

`rustc --version` in `nix develop` must report 1.95.0. A newer ambient
compiler is not the MSRV. `rust-version` in `cli/Cargo.toml` is `1.95`;
`rust-toolchain.toml` pins `1.95.0` with rustfmt and clippy. The Nix package
uses nixpkgs `rustPlatform` from rev `21a67dc470149f337cecafbe965d8d252a390518`.

`fleet --config PATH config validate` is the config bridge: exit 0 valid,
exit 2 invalid, no SSH or supervisor spawn. Help, version, and completions
succeed with an empty HOME.

Keep source control local until a commit is requested. Host activation, live
launchctl, and publishing are separate approvals.

## Layout

- `Cargo.toml`, `Cargo.lock`: Cargo workspace; root Cargo commands cover `cli/`
- `cli/src/` and `cli/tests/`: CLI, library, and integration tests
- `cli/examples/config.toml`: manual file, `supervisor = "none"`
- `apps/cliproxy-ui/`: React/Vite management UI, built to one HTML file
- `nix/packages/fleet.nix`: both binaries, completions, wrapped PATH
- `nix/packages/cliproxy-ui.nix`: `$out/share/cliproxy-ui/management.html`
- `nix/modules/home-manager.nix`: `programs.fleet.enable`, `.package`, snake_case `.settings`
- `.github/workflows/check.yml`: Linux and macOS Cargo; UI verify; Nix checks

Each package's Nix source is scoped to its own directory, so a UI edit does
not rebuild the CLI and the reverse. The UI version string is
`cliproxy-ui-<Fleet short rev>`, supplied by the flake. Never derive it from
git tags; those version the CLI.

`fleet` is the public CLI. `fleet-tunnel-runner` is the supervisor entry
point: local port, then SSH arguments. Keep that argv stable.

New public examples use fictional names and temporary paths.
