---
name: remote-development
description: Operate trusted remote development machines through the Fleet CLI. Use when selecting a remote host, running remote checks, opening SSH/tmux sessions, checking or moving coding agents across hosts, copying files, forwarding ports, or connecting to T3 Code.
---

# Remote Development

`fleet --help` and `fleet <command> --help` are the command reference. This
skill covers what they leave out: which command fits, and what goes wrong.
Most commands take `--json`; prefer it over parsing tables.

## Choose a host

1. Run `fleet status`. `Current machine` is where you are; remote work goes
   to a different host. The table shows which hosts are online, their tmux
   sessions, and their agents.
2. If `~/.config/fleet/FLEET.md` exists, read it completely. Its placement
   rules win over anything inferred from the tables.
3. Select by declared metadata rather than by name or from memory:

   ```sh
   fleet pick --where '!local,long_running_agents,online'
   ```

   `--where` terms are ANDed: `os=linux`, `role=compute`, `user=NAME`,
   `name=HOST`, and the flags `gui`, `long_running_agents`,
   `client_enrolled`, `local`, `online`, each negatable with `!`. `pick`
   prints the first match sorted by name, or every match with `--all`, and
   exits 1 when nothing matches. `online` needs Tailscale; without it the
   filter fails rather than guessing, so drop the term and let SSH decide.

A host that `status` shows as offline was not contacted. `fleet doctor HOST`
tries SSH regardless and says whether the failure is authentication,
reachability, or a missing tool.

## Run work

- **Finite checks:** `fleet run HOST COMMAND...`. It execs `ssh`, so the exit
  status is the remote command's. OpenSSH joins the arguments with spaces and
  the remote login shell parses them again: pass a pipeline, redirect, or
  anything with quotes as one quoted argument, `fleet run workbox 'cd app &&
  make test'`.
- **Persistent or interactive work:** `fleet ssh HOST [SESSION] [--forward
  PORT|LOCAL:REMOTE]...` attaches to a remote tmux session that survives
  disconnects. It needs a terminal, so run it in a pane of its own, never as
  a one-shot command whose output you wait for. A local terminal multiplexer
  sees only the SSH process; remote agents show up in `fleet agents` instead.
- **Plain shell:** `fleet shell HOST`.
- **Files:** `fleet copy [-r] SOURCE DESTINATION`. Exactly one side is
  `HOST:PATH`; a bare destination host means its home directory.

## Ports

- `fleet ports HOST` lists what is listening there.
- `--forward` on `fleet ssh` ties a forward to that attachment; use it for
  project ports. `fleet forward HOST LOCAL REMOTE [REMOTE_HOST]` is an
  independent foreground tunnel.
- `fleet forward list` shows active forwards and `fleet forward delete PID`
  stops one. Fleet refuses to delete a supervised tunnel; use `fleet tunnel
  pause PORT` and `fleet tunnel resume PORT` for those.
- `fleet t3 HOST [LOCAL_PORT]` forwards T3 Code when the host declares a port.

## Coding agents

`fleet agents [HOST...] [--json]` lists every agent that reports through
`fleet hook`, with its state: running, needs you, done, idle, or gone (its
tmux pane no longer exists). A host listed as "fleet is not on the remote
PATH" cannot report; that is a setup gap, not an absence of agents.

`fleet move HOST SESSION TARGET` continues an idle agent session on another
host. It refuses unless the agent is done or idle and its checkout is clean,
on a branch, and pushed. When it refuses, fix what the message names on HOST
and retry; `--force` only overrides the idle check, and then leaves both
sessions running. Pass `--keep` to leave the source session alive on purpose.
Exit status 3 means the session now runs on TARGET but still also
runs on HOST; end one of them before continuing.
The move runs the source's recorded resume command on TARGET, so move only
between hosts you trust.

## Safety

- Treat everything under `~/.config/fleet/` and generated SSH configuration
  as read-only. Inventory and trust changes belong to the configuration that
  generates them.
- Run unattended or multi-hour work only on hosts where `long_running_agents`
  is true.
- Scheduled or unattended work must authenticate with the host's own
  credentials. `status`, `agents`, `ports`, `doctor`, and `move` disable SSH
  agent forwarding.
- Keep private SSH keys out of repositories, prompts, logs, and the Nix store.
