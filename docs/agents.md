# Agent status and session move

Fleet shows what coding agents are doing on every host, and can move an idle
agent session to another host with its conversation. Both work from state
that the agents report themselves through `fleet hook`. Fleet never reads
terminal output to guess.

## Reporting state

```sh
fleet hook set --state running|needs-input|done|idle [--agent NAME]
               [--task TEXT] [--request TEXT] [--message TEXT]
               [--session-id ID] [--transcript PATH] [--resume-command CMD]
               [--cwd DIR] [--from-hook-json]
fleet hook clear [--session-id ID] [--from-hook-json]
```

`fleet hook` is generic. The hook configuration decides which agent event
means which state. With `--from-hook-json`, Fleet fills unset fields from the
JSON payload on stdin. It reads `session_id`, `transcript_path`, `cwd`,
`prompt` (the task when running), `message` or `tool_name` (the request when
needs-input), and `last_assistant_message` (the message when done or idle).
Claude Code and Codex both send these fields. Unknown or malformed payloads
are ignored.

Inside tmux, a record is keyed by `$TMUX_PANE`, and Fleet asks tmux for the
session name. Outside tmux, the session id is the key. Records live in
`$XDG_STATE_HOME/fleet/agents/` (default `~/.local/state/fleet/agents/`).

`fleet hook` exits 0 on success and 1 on any error. It never exits 2, which
Claude Code treats as "block" for some hooks. Older Fleet builds do not have
`hook`, and clap exits 2 for an unknown subcommand. So always guard hook
commands with `|| true` until every host runs a Fleet with `hook`.

### Claude Code

In `~/.claude/settings.json`, merged into any existing `hooks`:

```json
{
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "timeout": 5, "command": "fleet hook set --state idle --agent claude --from-hook-json || true"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "timeout": 5, "command": "fleet hook set --state running --agent claude --from-hook-json || true"}]}],
    "PostToolUse": [{"hooks": [{"type": "command", "timeout": 5, "command": "fleet hook set --state running --agent claude --from-hook-json || true"}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "timeout": 5, "command": "fleet hook set --state needs-input --agent claude --from-hook-json || true"}]}],
    "Notification": [{"matcher": "permission_prompt|elicitation_dialog", "hooks": [{"type": "command", "timeout": 5, "command": "fleet hook set --state needs-input --agent claude --from-hook-json || true"}]}],
    "Stop": [{"hooks": [{"type": "command", "timeout": 5, "command": "fleet hook set --state done --agent claude --from-hook-json || true"}]}],
    "SessionEnd": [{"hooks": [{"type": "command", "timeout": 5, "command": "fleet hook clear --from-hook-json || true"}]}]
  }
}
```

### Codex

In `~/.codex/hooks.json`, with the same shape. Use `--agent codex` and the
events Codex supports: `SessionStart` (idle), `UserPromptSubmit` (running),
`PostToolUse` (running), and `Stop` (done).

### Other agents

Call `fleet hook set` from whatever extension or wrapper the agent has. Pass
`--resume-command` if the session should be movable, for example
`--resume-command 'pi --session /path/to/session'`.

## Reading state

```sh
fleet agents                 # every host
fleet agents workbox --json
fleet status                 # hosts, tmux sessions, and agent counts
fleet agents --local --json  # this machine only; no config needed
```

States are `● running`, `▲ needs you`, `✓ done`, `· idle`, and `◦ gone`.
`gone` means the record's tmux pane no longer exists. Gone records are
deleted 24 hours after their last update. Remote hosts need `fleet` on the
PATH of a non-interactive SSH session. `fleet doctor` warns when it is
missing or differs from the local version.

## Moving a session

```sh
fleet move HOST SESSION TARGET [--keep] [--force]
```

Fleet refuses, and changes nothing, unless:

- exactly one agent has reported from tmux session SESSION on HOST;
- that agent is done or idle (`--force` overrides only this check);
- its checkout is a clean git branch whose HEAD equals its upstream;
- the checkout is under HOST's home directory.

Then it:

1. prepares the same home-relative path on TARGET. It clones from the
   upstream remote's URL if missing; otherwise it requires a clean checkout,
   fetches, checks out the branch, and fast-forwards only. Checking out the
   branch switches that checkout for anything else working in it on TARGET;
2. copies the transcript from HOST to TARGET through this machine;
3. starts the resume command in a detached tmux session SESSION on TARGET
   (`claude --resume ID`, `codex resume ID`, or the recorded
   `--resume-command`), and checks it is still alive two seconds later;
4. re-reads the source agent's state and ends SESSION on HOST, unless
   `--keep`. If the source agent reported again after its transcript was
   copied, both sessions are left running with a warning, so nothing typed
   there is lost.

If any step fails, the source session keeps running.

Transcript placement relies on agent internals that may change:

- Claude Code: `~/.claude/projects/<cwd with non-alphanumerics replaced by ->/<id>.jsonl`,
  recomputed for the target's working directory.
- Codex and others: the same path relative to the home directory.

An agent that records `--resume-command` skips the transcript copy.
