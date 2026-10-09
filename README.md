# Fleet

A command-line tool for working across a handful of your own machines: a
laptop, a desktop, a home server, a cloud box. Typically they're all on one
[Tailscale](https://tailscale.com) network.

You declare your machines once. Fleet then lets you:

- **See everything at once:** which machines are online, their tmux
  sessions, and what your coding agents (Claude Code, Codex, …) are doing on
  each one.
- **Jump in:** attach to a machine's persistent tmux session, open a shell,
  or run a one-off command.
- **Move work around:** copy files, forward ports, keep localhost tunnels
  alive, and hand an idle agent session, with its whole conversation, to
  another machine.

Fleet sits on tools you already trust: **OpenSSH** for every connection,
**tmux** for persistent sessions, and **launchd** on macOS to supervise
tunnels. There is no daemon, no server, and no agent to install on the
remote side. Your `~/.ssh/config`, keys, and Tailscale setup keep working
exactly as they do today.

```console
$ fleet status
Current machine: laptop

HOST               NETWORK    PATH                  FLEET     SESSIONS                     AGENTS
laptop             self       -                     0.1.0     main*                        -
workbox            online     direct                0.1.0     main,agents                  ▲ needs you 1, ✓ done 1
homeserver         offline    seen 2026-10-01 08:30 -         offline                      ?

HOST               SESSION        STATE         AGENT    AGE   DETAIL
workbox            agents         ▲ needs you   claude   2m    permission: Bash
workbox            main           ✓ done        codex    40m   Release notes drafted
```

## A quick tour

```sh
fleet                                   # list declared hosts
fleet status                            # online state, tmux sessions, agents
fleet ssh workbox                       # attach to workbox's tmux session "main"
fleet ssh workbox agents --forward 5173 # another session, with a port forward
fleet run workbox make test             # one-off command over SSH
fleet copy -r ./site workbox:/tmp/site  # scp-style copy, through Fleet's names
fleet ports workbox                     # what is listening there
fleet forward workbox 3000 3000         # forward a port to localhost
fleet doctor                            # check SSH, tunnels, tmux on every host

# Pick a machine by what it is, not by name:
fleet run "$(fleet pick --where '!local,long_running_agents,online')" make test

# Coding agents across machines:
fleet agents                            # every agent, on every host
fleet move workbox agents laptop        # continue an idle agent session elsewhere
```

Most commands take `--json` for scripts and agents. Run `fleet --help` or
`fleet <command> --help` for the full reference.

## Install

Fleet runs on macOS (Apple Silicon) and Linux (x86_64). It needs OpenSSH and
tmux; the Nix package brings both.

**Nix**, to try it:

```sh
nix run github:maximilianpw/fleet/v0.1.0 -- --help
```

**Nix flake input**, with the Home Manager module that writes the config for
you:

```nix
{
  inputs.fleet.url = "github:maximilianpw/fleet/v0.1.0";

  # in a Home Manager configuration:
  imports = [inputs.fleet.homeManagerModules.default];
  programs.fleet = {
    enable = true;
    package = inputs.fleet.packages.${pkgs.stdenv.hostPlatform.system}.fleet;
    settings = { /* same fields as config.toml below */ };
  };
}
```

**Cargo** (Rust 1.95 or newer):

```sh
cargo install --locked --git https://github.com/maximilianpw/fleet --tag v0.1.0 fleet
```

Shell completions: `fleet completions bash|zsh|fish`. The Nix package
installs them for you.

## Configure

Fleet reads one TOML file, by default `~/.config/fleet/config.toml`. Each host
names the SSH destination Fleet should use, so anything `ssh` can reach works:
a Tailscale MagicDNS name, an `~/.ssh/config` alias, or an IP address.

```toml
schema_version = 1
current_host = "laptop"          # which entry is this machine

[hosts.laptop]
ssh_target = "laptop"
aliases = []
os = "darwin"
role = "interface"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = false

[hosts.workbox]
ssh_target = "workbox"           # e.g. a MagicDNS name or ssh alias
aliases = ["dev"]
os = "linux"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = true       # OK for unattended agent work
```

Check it with `fleet config validate`. The fields, optional overrides (tmux
command and session, forward targets, T3 Code port, Tailscale name), and
supervised tunnels are described in
[docs/configuration.md](docs/configuration.md). A commented example lives in
[cli/examples/config.toml](cli/examples/config.toml).

Tailscale is optional. With it, `fleet status` shows whether each host is
online and whether the connection is direct or relayed, and `--where online`
filters by it. Without it, everything else works the same.

## Coding agents

Fleet doesn't run agents. It shows what they're doing and moves them. Each
agent reports its state through a hook that calls `fleet hook set --state
running|needs-input|done|idle`. Claude Code and Codex hooks can pass their
event JSON straight through. `fleet agents` and `fleet status` then show every
agent on every machine, with what it's working on or waiting for.

`fleet move HOST SESSION TARGET` hands an idle agent to another machine. It
first checks that the work is committed and pushed. It then checks out the
same branch on the target, copies the conversation transcript, and resumes
the agent in a tmux session of the same name there. The original session ends
only once the new one is running.

Hook setup and the exact move rules are in [docs/agents.md](docs/agents.md).

## How it works

- **Every connection is plain OpenSSH.** `fleet ssh`, `shell`, `run`, `copy`,
  and `forward` replace themselves with `ssh` or `scp`. Signals, exit codes,
  and terminal handling are OpenSSH's own.
- **Multi-host queries run in parallel** (`status`, `agents`, `doctor`,
  `ports`, `move`). Each sends one POSIX `sh` script per host. Repeated
  queries to a host share one SSH connection for 60 seconds. The scripts are
  quoted to work behind bash, zsh, or fish login shells.
- **Tunnels on macOS** can be supervised by launchd with automatic
  reconnects, and paused or resumed with `fleet tunnel`.
- **State is minimal:** one config file, agent state records under
  `~/.local/state/fleet/`, and SSH control sockets. Fleet never edits your
  SSH config.

## Repository layout

This repository is a small monorepo:

| Path | What |
| --- | --- |
| `cli/` | The Fleet CLI (Rust) and its tests |
| `nix/` | Nix packages, the Home Manager module, NixOS modules and tests |
| `apps/cliproxy-ui/` | A management UI for CLIProxyAPI, packaged as one HTML file |
| `services/cliproxy-quota/` | A quota endpoint for CLIProxyAPI ([README](services/cliproxy-quota/README.md)) |
| `docs/` | Configuration, agents, compatibility, and migration notes |

The CLIProxy pieces are packaged here for convenience and are independent of
the CLI.

## Development

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
nix build path:$PWD#fleet --no-link
alejandra --check flake.nix nix
```

The tests never contact real hosts. They use temporary home directories and
fake `ssh`, `tmux`, and `tailscale` executables. CI runs the Cargo suite on
Linux and macOS, plus the Nix checks for every package. The full check list
is in [AGENTS.md](AGENTS.md).

## Docs

- [Configuration](docs/configuration.md)
- [Agent status and session move](docs/agents.md)
- [Compatibility](docs/compatibility.md): the behavior Fleet guarantees and
  why
- [Migration baseline](docs/migration-baseline.md) and
  [monorepo migration plan](docs/monorepo-migration-plan.md)

## License

Fleet itself has no license file. That is a deliberate decision by the owner,
not an omission, so default copyright applies. The UI in `apps/cliproxy-ui/`
keeps its upstream MIT license.
