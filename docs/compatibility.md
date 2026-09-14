# Compatibility

This extraction keeps the Bash Fleet command contracts. Matching mocked tests
does not inherit live Joyce/Kim acceptance from Plan 001.

## Preserved

- No arguments runs `list`. Help text, command aliases, display fields, and
  row ordering stay the same.
- `ssh` / tmux session behavior, local-host behavior, and ad-hoc forwarding.
- Foreground attachment: stdin, TTY, exit status, INT/TERM. Unix exec
  replacement is used for plain shell and SSH attachment where practical.
  `Command::status` is not a substitute when it would leave a wrapper with
  different signal semantics.
- No agent forwarding. Managed loopback binds. No multiplexing on managed and
  attachment-scoped forwards.
- launchd pause persistence, unrelated-listener protection, 45s startup
  deadline, and doctor distinctions.
- `run`: local command uses argv; remote command follows OpenSSH remote-shell
  joining. Spaces, quoting, and a fish remote login shell are part of the
  contract. There is no silent argv-safe remote mode in v1.
- `shell HOST` accepts trailing SSH arguments, including option-like ones
  after HOST. Local-host shell ignores extras.
- Local `ssh` uses PATH tmux and session `main`, independent of remote
  metadata.
- Ad-hoc `forward` keeps existing remote-target string handling, including
  bracketed forms passed through inside one `-L` argument.
- Unknown SSH aliases keep command-specific fallbacks: `ssh unknown` uses
  `tm-unknown`; shell/run use `unknown`; forward uses `fleet-forward-unknown`;
  doctor, t3, and copy still require declared metadata.
- Managed job labels stay `org.nix-community.home.fleet-tunnel-PORT`. Pause
  intent lives in launchd against those labels. Renaming them is a migration.

## Permitted v1 additions

- `--version`
- `completions SHELL`
- `config validate`
- `copy SOURCE DESTINATION` for one local-to-remote or remote-to-local file

## Permitted differences

- Config and setup diagnostics. Missing explicit or default config is an
  error, not a silent hop to another file.
- Consistent early rejection of invalid or out-of-range ports and option-like
  host targets.
- Managed-process delete protection. Identify the managed job or its direct
  SSH child, not just a matching port number. `fleet tunnel pause PORT` is the
  guidance for a verified managed forward. PID deletion of that process is
  refused. A paused managed port occupied by an unrelated ad-hoc forward is
  not labeled owned. Supervisor `none` keeps unmanaged behavior. If an active
  launchd mapping may own a candidate but its snapshot cannot be read,
  deletion is refused rather than guessed.

No other silent redesign.

## File copy

`fleet copy` uses OpenSSH `scp` in its default SFTP-backed mode. Fleet does not
pass `-O` or opt into the legacy SCP protocol. It resolves a canonical host or
alias to `ssh_target`, passes paths as argv after `--`, and exec-replaces Fleet
so transfer diagnostics, signals, and exit status come from `scp`.

The first version copies one file and requires exactly one local endpoint and
one declared remote Fleet endpoint. A bare remote destination means its home
directory. Pull sources require `HOST:PATH`. Recursive and remote-to-remote
copy are out of scope. `CURRENT_HOST:PATH` is normalized to a local path.
Local filenames containing a colon should use an explicit path prefix such as
`./report:final.md` so they are not parsed as Fleet remote syntax.

Remote path semantics and limitations belong to the installed OpenSSH `scp`
implementation. Fleet rejects empty and control-character paths, but does not
add a second quoting language or construct a local shell command.

## Existing port conflicts

This extraction does not resolve port conflicts with unrelated local or remote
services. Host-wide `fleet doctor` can fail while one mapping works correctly.
Doctor distinguishes TCP listening from expected-app readiness. A local
listener is not end-to-end health. Remote probes still use Bash plus `timeout`
on the remote host.

## Not in this package

- Herdr orchestration. Plan 001 proved native Herdr sessions. This CLI does
  not start or manage them.
- Workspace state, dynamic tunnel installation, or a second supervisor.
- Personal inventory, SSH keys, known hosts, or `FLEET.md`.
- Windows.

Nix-wrapped binaries get OpenSSH, tmux, lsof, and Linux process tools on PATH.
A Cargo-installed binary reports missing SSH, tmux, process-list, and signal
tools when an operation needs them. A failed lsof ownership probe stays
`unknown`, so Fleet refuses unsafe tunnel mutations rather than guessing.
`launchctl` stays an OS command. Remote doctor probes still require Bash and
GNU `timeout` on the remote host.

## Platform gate

Automated tests use fictional hosts, temporary HOME/config directories, fake
tools, or task-created child processes. They must not contact real hosts, use
live launchctl, or signal existing user processes.

Linux CI can evaluate Darwin Home Manager outputs with a placeholder package
and must not build a Darwin Rust package to do that. A Linux-only pass is not
Darwin runtime verification. Until macOS package and launchd tests actually
run, the cross-platform gate is BLOCKED.

Installed cutover is a later approval. The Rust runner changes LaunchAgent
`ProgramArguments` and may restart existing jobs on activation. That is not
disruption-free. Keep the labels.
