# Configuration

Fleet reads one TOML file. It does not merge files, guess the current host from
the machine hostname, or write config back.

## Lookup order

1. `--config PATH`
2. `FLEET_CONFIG`
3. `$XDG_CONFIG_HOME/fleet/config.toml`
4. `$HOME/.config/fleet/config.toml`

A missing `--config` or `FLEET_CONFIG` path is an error. A missing default file
is a setup error for operational commands. `help`, `--version`, and
`completions` work with an empty HOME.

```sh
fleet --config ./examples/config.toml config validate
```

Exit 0 if the file is valid. Exit 2 if it is invalid. The command does not
spawn SSH, tmux, launchctl, or other operational subprocesses.

## Schema

`schema_version` must be `1`. Unknown fields fail. Required top-level fields
are `schema_version`, `current_host`, and `hosts`. `tunnels` defaults to
`supervisor = "none"` and no mappings.

`current_host` is a canonical key in `hosts`. Local vs remote dispatch uses
that key and its aliases. It is not inferred.

### Hosts

Required host fields:

| Field | Meaning |
| --- | --- |
| `ssh_target` | OpenSSH destination or alias. One argument. No leading `-`, no control characters. |
| `aliases` | Alternate names. Unique across canonical keys and aliases. |
| `os` | Free-form metadata. Unknown strings are allowed. |
| `role` | Shown by `list`. |
| `user` | Display metadata. Not an SSH `User` override. OpenSSH still owns login, port, and identity. |
| `client_enrolled` | Outbound identity enrolled. `list` prints yes or no. |
| `gui` | GUI or screenshot surface. |
| `long_running_agents` | Whether unattended agent work should run here. |

Optional host fields, with runtime defaults:

| Field | Default |
| --- | --- |
| `display_target` | `ssh_target` |
| `tmux_target` | unset; Fleet runs an explicit remote tmux command through `ssh_target` |
| `forward_target` | `ssh_target` |
| `tmux_command` | `tmux` |
| `tmux_session` | `main` |
| `t3code_port` | unset; `fleet t3` requires it |
| `alias_targets.<alias>` | inherit canonical targets |

Nix sets `display_target` to the inventory hostname so `list` can show the
real target while `ssh_target` stays a short alias. When `tmux_target` is set,
the no-session SSH path uses it. That is how generated `tm-HOST` aliases keep
working. Otherwise Fleet builds an explicit remote tmux command.

`alias_targets` keys must be declared aliases. Each record may override
`ssh_target`, `tmux_target`, and `forward_target`. Unspecified fields inherit
the canonical host. Ordinary manual configs can omit the table. Nix emits
explicit triples for every remote alias so `fleet ssh dev` can stay
`ssh tm-dev` instead of silently becoming `ssh tm-workbox`.

Local `fleet ssh` uses PATH `tmux` and session `main`, even if remote metadata
names another executable or session.

### Tunnels

```toml
[tunnels]
supervisor = "none" # or "launchd"
mappings = []
```

A mapping is:

| Field | Meaning |
| --- | --- |
| `host` | Remote canonical name or alias. Never the current host. |
| `local_port` | `1..65535`, unique among mappings. |
| `remote_port` | `1..65535`. |
| `remote_host` | DNS or IPv4-ish name. Default `localhost`. IPv6 literals are out of scope. |
| `label` | launchd identifier. Home Manager defaults to `org.nix-community.home.fleet-tunnel-PORT`. |

Plist paths come from the runtime `HOME`, not a path baked into the package.
Labels use a restricted identifier syntax. Renaming a label is a behavioral
change: launchd stores pause state against the label, outside the plist.

`supervisor = "none"` may still list mappings as unsupervised. It does not
own processes. Ad-hoc `forward` remote targets are data inside one `-L`
argument and may use bracketed address forms. Mapping and doctor targets use
the stricter DNS/IPv4 check above.

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

Field names are the TOML names. There is no camelCase translation layer.

The module writes `xdg.configFile."fleet/config.toml"` and installs exactly
one `fleet` package. On Darwin, `tunnels.supervisor = "launchd"` also
declares `launchd.agents."fleet-tunnel-PORT"` with Label
`org.nix-community.home.fleet-tunnel-PORT`. Linux configs do not install
those jobs.

After a package transition changes a job's `ProgramArguments`, launchd may
leave the replacement job loaded but stopped or uninitialized. Inspect
`launchctl print gui/$UID/org.nix-community.home.fleet-tunnel-PORT` and verify
a fresh, non-multiplexed SSH connection first. `fleet tunnel resume PORT` then
enables and kickstarts that exact loaded job; it does not delete the plist or
kill an unrelated listener. Needing this on an unchanged later activation is a
launchd activation defect rather than expected steady state.

It does not generate SSH identities, overwrite `~/.ssh/config`, register
Herdr machines, or install public keys. Personal inventory, trust, and
`hosts.json` / `FLEET.md` exports stay in the consumer.

### Nix projection

| Personal source | Runtime field |
| --- | --- |
| hostname / inventory key | `current_host` / hosts table key |
| remote inventory key | `ssh_target`, never `host.hostName` |
| `host.hostName` | `display_target` |
| `host.user`, role, aliases, os, gui | `user`, `role`, `aliases`, `os`, `gui` |
| `host.client != null` | `client_enrolled` |
| `host.longRunningAgents` | `long_running_agents` |
| `host.tmuxCommand`, `tmuxSession`, optional `t3codePort` | `tmux_command`, `tmux_session`, optional `t3code_port` |
| remote canonical key or alias token | `ssh_target = token`, `tmux_target = tm-token`, `forward_target = fleet-forward-token`, with `alias_targets` for aliases |
| `localInventoryHost.darwin` | `tunnels.supervisor` `launchd` or `none` |
| mapping host, localPort, remotePort, remoteHost or localhost | `host`, `local_port`, `remote_port`, `remote_host` |
| mapping localPort | `label` `org.nix-community.home.fleet-tunnel-PORT` |

`list` keeps column order `HOST USER TARGET ROLE CLIENT ALIASES`, sorts
canonical names, joins aliases with commas and no spaces, and prints client
enrollment as `yes` or `no`.

## Validation before spawn

Fleet rejects, before starting a subprocess:

- unsupported schema version or unknown fields
- duplicate aliases or missing required fields
- ports outside `1..65535` or duplicate managed local ports
- mapping host that is undeclared or is the current host
- option-like SSH targets and control characters
- unsafe remote-host syntax on managed and doctor targets

SSH targets are individual arguments. Configuration text is not expanded as
shell.
