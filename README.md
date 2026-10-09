# Fleet

CLI for a small SSH/tmux development fleet. OpenSSH is the transport for
sessions, forwards, and direct file copy. tmux is the `fleet ssh` backend.
launchd is the current managed-tunnel supervisor.

This repository packages Fleet independently from any personal Nix
configuration. The crate is not published to crates.io, and installing this
source does not switch an existing host configuration.

The repository is a monorepo. The CLI lives in `cli/`. The CLIProxy
management UI lives in `apps/cliproxy-ui/` and is packaged as
`packages.<system>.cliproxy-ui`, a single
`share/cliproxy-ui/management.html`. The CLIProxy quota service lives in
[`services/cliproxy-quota/`](services/cliproxy-quota/README.md), packaged as
`packages.<system>.cliproxy-quota` with `nixosModules.cliproxy-quota`. See
[docs/monorepo-migration-plan.md](docs/monorepo-migration-plan.md).

## Install

Cargo:

```sh
cargo install --locked --path cli --root "$PWD/target/fleet-prefix"
```

Nix:

```sh
nix build path:$PWD#fleet
```

The Nix package wraps `fleet` and `fleet-tunnel-runner` with OpenSSH, tmux,
lsof, and Linux process tools. Completions for bash, zsh, and fish are
installed next to the binaries.

If `fleet --version` looks right but behavior does not, check which executable
is first on PATH:

```sh
type -a fleet
```

## Home Manager

```nix
{
  imports = [inputs.fleet.homeManagerModules.default];
  programs.fleet = {
    enable = true;
    package = inputs.fleet.packages.${pkgs.stdenv.hostPlatform.system}.fleet;
    settings = {
      schema_version = 1;
      current_host = "laptop";
      hosts = {
        laptop = {
          ssh_target = "laptop";
          aliases = [];
          os = "darwin";
          role = "interface";
          user = "developer";
          client_enrolled = true;
          gui = true;
          long_running_agents = false;
        };
        workbox = {
          ssh_target = "workbox";
          aliases = ["dev"];
          os = "linux";
          role = "compute";
          user = "developer";
          client_enrolled = true;
          gui = false;
          long_running_agents = true;
        };
      };
      tunnels.supervisor = "none";
    };
  };
}
```

Settings use the TOML field names. The module writes
`xdg.configFile."fleet/config.toml"`. Darwin launchd jobs are optional and
only exist when `tunnels.supervisor = "launchd"`. See
[docs/configuration.md](docs/configuration.md).

A manual non-Nix file lives at [cli/examples/config.toml](cli/examples/config.toml).
Validate it with:

```sh
fleet --config ./cli/examples/config.toml config validate
```

## Commands

`fleet` with no arguments lists hosts. Other commands: `status`, `agents`,
`pick`, `ssh`, `shell`, `run`, `copy`, `ports`, `forward`, `t3`, `tunnel`,
`doctor`, `move`, `hook`, `config validate`, `completions`.

See every host at once:

```sh
fleet status                                  # presence, tmux sessions, agents
fleet status --where long_running_agents --json
fleet doctor                                  # all hosts, concurrently
fleet run "$(fleet pick --where '!local,long_running_agents')" make test
fleet ports workbox                           # listening ports to forward
```

`--where` takes a comma-separated AND of `key=value`, `key!=value`, `flag`,
or `!flag`. Keys are `name`, `os`, `role`, `user`, `gui`,
`long_running_agents`, `client_enrolled`, `local`, and `online`, which needs
Tailscale. `--json` is available on `list`, `status`, `agents`, `doctor`,
`ports`, `tunnel status`, and `forward list`.

Agents report state with `fleet hook`. `fleet move workbox agents laptop`
moves an idle agent session, with its conversation, to another host. See
[docs/agents.md](docs/agents.md).

Copy one file to or from a declared remote host with scp-style endpoints:

```sh
fleet copy report.md workbox
fleet copy report.md workbox:/tmp/report.md
fleet copy workbox:/tmp/report.md .
```

Canonical host names and aliases resolve through Fleet configuration. A bare
remote destination means that host's home directory. Exactly one endpoint must
be local; `-r` copies directories, and remote-to-remote copy is not supported. Fleet execs
OpenSSH `scp` directly, passes paths as argv after `--`, and relies on its
default SFTP-backed transfer mode. Fleet does not enable legacy SCP protocol
mode (`scp -O`).

Help, version, and completions do not need a config file.

## Checks

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
nix build path:$PWD#fleet path:$PWD#checks.x86_64-linux.fleet path:$PWD#checks.x86_64-linux.home-manager --no-link
alejandra --check flake.nix nix
nix build path:$PWD#cliproxy-ui path:$PWD#checks.x86_64-linux.cliproxy-ui --no-link
nix build path:$PWD#cliproxy-quota path:$PWD#checks.x86_64-linux.cliproxy-quota path:$PWD#checks.x86_64-linux.cliproxy-quota-nixos --no-link
```

Supported package outputs are `x86_64-linux` and `aarch64-darwin`. The Linux
Home Manager check evaluates Darwin module outputs with a placeholder package
and does not build a Darwin Rust binary.

## Docs

- [Configuration](docs/configuration.md)
- [Agent status and session move](docs/agents.md)
- [Compatibility](docs/compatibility.md)
- [Migration baseline](docs/migration-baseline.md)
- [Monorepo migration plan](docs/monorepo-migration-plan.md)

No root license file. That is an owner decision, not an omission. The UI in
`apps/cliproxy-ui/` keeps its upstream MIT license.
