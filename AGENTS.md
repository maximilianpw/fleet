# Fleet

Standalone Rust CLI plus optional Home Manager module. OpenSSH, tmux, and
launchd stay the backends. Personal inventory and trust stay in the consumer
Nix config.

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
Licensing stays unset. There is no LICENSE file.

## Commands

From the repo root, with `Cargo.lock` present. Done means exit 0.

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

`rustc --version` in `nix develop` must report 1.95.0. A newer ambient
compiler is not the MSRV. `rust-version` in Cargo.toml is `1.95`;
`rust-toolchain.toml` pins `1.95.0` with rustfmt and clippy. The Nix package
uses nixpkgs `rustPlatform` from rev `21a67dc470149f337cecafbe965d8d252a390518`.

`fleet --config PATH config validate` is the config bridge: exit 0 valid,
exit 2 invalid, no SSH or supervisor spawn. Help, version, and completions
succeed with an empty HOME.

Generate `Cargo.lock` before any `--locked` command. Keep source control
local until a commit is requested. Host activation, live launchctl, and
publishing are separate approvals.

## Layout

- `src/` and `tests/`: CLI, library, and integration tests
- `nix/package.nix`: both binaries, completions, wrapped PATH
- `nix/home-manager.nix`: `programs.fleet.enable`, `.package`, snake_case `.settings`
- `examples/config.toml`: manual file, `supervisor = "none"`
- `.github/workflows/check.yml`: Linux and macOS Cargo; Linux Nix checks

`fleet` is the public CLI. `fleet-tunnel-runner` is the supervisor entry
point: local port, then SSH arguments. Keep that argv stable.

New public examples use fictional names and temporary paths.
