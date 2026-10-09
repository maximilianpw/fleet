//! Agent state reported by hooks: `fleet hook set|clear` and local records.
//!
//! Fleet does not know any agent's hook events. The hook configuration maps
//! events to states and calls `fleet hook set --state ...`, so Claude Code,
//! Codex, Pi, or a script can all report the same way.
//!
//! One JSON record per agent lives in `$XDG_STATE_HOME/fleet/agents/`
//! (default `~/.local/state/fleet/agents/`), keyed by tmux pane when the agent
//! runs inside tmux, else by session id. Records are replaced atomically.
//!
//! Hooks must never block the agent. `fleet hook` exits 0 on success and 1 on
//! any error, never 2: Claude Code treats exit 2 from some hooks as "block".

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::process::run_with_deadline;

const RECORD_VERSION: u32 = 1;
const TEXT_LIMIT: usize = 240;
const TMUX_TIMEOUT: Duration = Duration::from_secs(3);
/// Records whose pane disappeared are deleted after this long.
const GONE_RETENTION_SECS: u64 = 24 * 60 * 60;

pub const HOOK_USAGE: &str = "\
usage:
  fleet hook set --state running|needs-input|done|idle [--agent NAME]
                 [--task TEXT] [--request TEXT] [--message TEXT]
                 [--session-id ID] [--transcript PATH] [--resume-command CMD]
                 [--cwd DIR] [--from-hook-json]
  fleet hook clear [--session-id ID] [--from-hook-json]

--from-hook-json reads session_id, transcript_path, cwd, prompt, message,
tool_name, and last_assistant_message from a Claude Code or Codex hook payload
on stdin. Explicit flags win over payload fields.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentState {
    Running,
    NeedsInput,
    Done,
    Idle,
    /// The record's tmux pane no longer exists. Never written by hooks.
    Gone,
}

impl AgentState {
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "running" | "working" => Self::Running,
            "needs-input" | "needs_input" | "waiting" => Self::NeedsInput,
            "done" => Self::Done,
            "idle" => Self::Idle,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "● running",
            Self::NeedsInput => "▲ needs you",
            Self::Done => "✓ done",
            Self::Idle => "· idle",
            Self::Gone => "◦ gone",
        }
    }

    /// True while the agent is mid-turn or blocked on the user.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Running | Self::NeedsInput)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub version: u32,
    pub key: String,
    pub agent: String,
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane: Option<String>,
    pub updated_at: u64,
}

impl AgentRecord {
    /// The most useful one-line description for the current state.
    pub fn detail(&self) -> &str {
        let pick = match self.state {
            AgentState::NeedsInput => self.request.as_ref().or(self.task.as_ref()),
            AgentState::Done | AgentState::Idle => self.message.as_ref().or(self.task.as_ref()),
            AgentState::Running | AgentState::Gone => self.task.as_ref(),
        };
        pick.map(String::as_str).unwrap_or("")
    }
}

/// Environment the hook and local listing read. Kept apart from
/// [`crate::process::ProcessEnv`], which every command constructs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEnv {
    pub home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub tmux_pane: Option<String>,
}

impl AgentEnv {
    pub fn from_os() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            home: non_empty("HOME").map(PathBuf::from),
            xdg_state_home: non_empty("XDG_STATE_HOME").map(PathBuf::from),
            tmux_pane: non_empty("TMUX_PANE").and_then(|value| value.into_string().ok()),
        }
    }

    pub fn state_dir(&self) -> Result<PathBuf, AgentError> {
        if let Some(state) = &self.xdg_state_home {
            return Ok(state.join("fleet/agents"));
        }
        self.home
            .as_ref()
            .map(|home| home.join(".local/state/fleet/agents"))
            .ok_or(AgentError::NoStateDir)
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("{HOOK_USAGE}")]
    Usage,
    #[error("fleet hook: {0}")]
    BadArgument(String),
    #[error("fleet hook: neither TMUX_PANE nor a session id is available to identify this agent")]
    NoIdentity,
    #[error("fleet: HOME and XDG_STATE_HOME are unset; cannot locate agent state")]
    NoStateDir,
    #[error("fleet: agent state I/O failed for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

impl AgentError {
    pub fn exit_code(&self) -> i32 {
        1
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookFields {
    pub agent: Option<String>,
    pub task: Option<String>,
    pub request: Option<String>,
    pub message: Option<String>,
    pub session_id: Option<String>,
    pub transcript: Option<String>,
    pub resume_command: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCommand {
    Set {
        state: AgentState,
        fields: HookFields,
    },
    Clear {
        session_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHook {
    pub command: HookCommand,
    pub from_hook_json: bool,
}

/// Parse `fleet hook ...` arguments by hand so malformed hook configuration
/// can never produce clap's exit status 2.
pub fn parse_hook_args(args: &[String]) -> Result<ParsedHook, AgentError> {
    let (verb, rest) = args.split_first().ok_or(AgentError::Usage)?;
    let mut state = None;
    let mut fields = HookFields::default();
    let mut from_hook_json = false;
    let mut index = 0;
    while index < rest.len() {
        let flag = rest[index].as_str();
        if flag == "--from-hook-json" {
            from_hook_json = true;
            index += 1;
            continue;
        }
        if matches!(flag, "-h" | "--help") {
            return Err(AgentError::Usage);
        }
        let value = rest
            .get(index + 1)
            .ok_or_else(|| AgentError::BadArgument(format!("{flag} requires a value")))?
            .clone();
        let slot = match flag {
            "--state" => {
                state =
                    Some(AgentState::parse(&value).ok_or_else(|| {
                        AgentError::BadArgument(format!("unknown state '{value}'"))
                    })?);
                index += 2;
                continue;
            }
            "--agent" => &mut fields.agent,
            "--task" => &mut fields.task,
            "--request" => &mut fields.request,
            "--message" => &mut fields.message,
            "--session-id" => &mut fields.session_id,
            "--transcript" => &mut fields.transcript,
            "--resume-command" => &mut fields.resume_command,
            "--cwd" => &mut fields.cwd,
            _ => return Err(AgentError::BadArgument(format!("unknown option '{flag}'"))),
        };
        *slot = Some(value);
        index += 2;
    }
    let command = match verb.as_str() {
        "set" => HookCommand::Set {
            state: state.ok_or_else(|| AgentError::BadArgument("set requires --state".into()))?,
            fields,
        },
        "clear" => HookCommand::Clear {
            session_id: fields.session_id,
        },
        _ => return Err(AgentError::Usage),
    };
    Ok(ParsedHook {
        command,
        from_hook_json,
    })
}

/// Fill fields the flags left unset from a hook payload. Unknown or
/// malformed payloads are ignored: a hook must not fail on a schema change.
pub fn merge_hook_payload(fields: &mut HookFields, state: Option<AgentState>, payload: &str) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(payload)
    else {
        return;
    };
    let text = |key: &str| {
        map.get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    };
    fill(&mut fields.session_id, text("session_id"));
    fill(&mut fields.transcript, text("transcript_path"));
    fill(&mut fields.cwd, text("cwd"));
    match state {
        Some(AgentState::Running) => fill(&mut fields.task, text("prompt")),
        Some(AgentState::NeedsInput) => fill(
            &mut fields.request,
            text("message").or_else(|| text("tool_name").map(|tool| format!("permission: {tool}"))),
        ),
        Some(AgentState::Done | AgentState::Idle) => {
            fill(&mut fields.message, text("last_assistant_message"))
        }
        _ => {}
    }
}

fn fill(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value;
    }
}

/// Run a parsed hook. Reads stdin only when `--from-hook-json` was given.
pub fn run_hook(parsed: ParsedHook, env: &AgentEnv) -> Result<(), AgentError> {
    let payload = if parsed.from_hook_json {
        let mut text = String::new();
        let _ = io::stdin().read_to_string(&mut text);
        Some(text)
    } else {
        None
    };
    let dir = env.state_dir()?;
    match parsed.command {
        HookCommand::Set { state, mut fields } => {
            if let Some(payload) = &payload {
                merge_hook_payload(&mut fields, Some(state), payload);
            }
            let tmux_session = env.tmux_pane.as_deref().and_then(tmux_session_of_pane);
            set_record(
                &dir,
                state,
                fields,
                env.tmux_pane.as_deref(),
                tmux_session,
                now_secs(),
            )?;
            Ok(())
        }
        HookCommand::Clear { mut session_id } => {
            if let Some(payload) = &payload {
                let mut fields = HookFields::default();
                merge_hook_payload(&mut fields, None, payload);
                fill(&mut session_id, fields.session_id);
            }
            clear_records(&dir, env.tmux_pane.as_deref(), session_id.as_deref())
        }
    }
}

/// Create or update the record for this pane or session.
pub fn set_record(
    dir: &Path,
    state: AgentState,
    fields: HookFields,
    tmux_pane: Option<&str>,
    tmux_session: Option<String>,
    now: u64,
) -> Result<AgentRecord, AgentError> {
    let key = record_key(tmux_pane, fields.session_id.as_deref())?;
    let previous = read_record(&dir.join(format!("{key}.json"))).filter(|old| {
        match (&old.session_id, &fields.session_id) {
            (Some(old_id), Some(new_id)) => old_id == new_id,
            _ => true,
        }
    });
    let keep = |new: Option<String>, old: Option<&String>| new.or_else(|| old.cloned());
    let old = previous.as_ref();
    let record = AgentRecord {
        version: RECORD_VERSION,
        key: key.clone(),
        agent: fields
            .agent
            .or_else(|| old.map(|record| record.agent.clone()))
            .unwrap_or_else(|| "agent".into()),
        state,
        task: keep(
            fields.task.map(|t| clip(&t)),
            old.and_then(|r| r.task.as_ref()),
        ),
        request: match state {
            AgentState::NeedsInput => fields.request.map(|t| clip(&t)),
            _ => None,
        },
        message: match state {
            AgentState::Done | AgentState::Idle => keep(
                fields.message.map(|t| clip(&t)),
                old.and_then(|r| r.message.as_ref()),
            ),
            _ => None,
        },
        session_id: keep(fields.session_id, old.and_then(|r| r.session_id.as_ref())),
        transcript: keep(fields.transcript, old.and_then(|r| r.transcript.as_ref())),
        resume_command: keep(
            fields.resume_command,
            old.and_then(|r| r.resume_command.as_ref()),
        ),
        cwd: keep(fields.cwd, old.and_then(|r| r.cwd.as_ref())).or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned())
        }),
        tmux_session: tmux_session.or_else(|| old.and_then(|r| r.tmux_session.clone())),
        tmux_pane: tmux_pane.map(str::to_string),
        updated_at: now,
    };
    write_record(dir, &record)?;
    Ok(record)
}

/// Remove this pane's record and every record with `session_id`.
pub fn clear_records(
    dir: &Path,
    tmux_pane: Option<&str>,
    session_id: Option<&str>,
) -> Result<(), AgentError> {
    if tmux_pane.is_none() && session_id.is_none() {
        return Err(AgentError::NoIdentity);
    }
    for (path, record) in read_dir_records(dir)? {
        let pane_match = tmux_pane.is_some() && record.tmux_pane.as_deref() == tmux_pane;
        let session_match = session_id.is_some() && record.session_id.as_deref() == session_id;
        if pane_match || session_match {
            remove(&path)?;
        }
    }
    Ok(())
}

fn record_key(tmux_pane: Option<&str>, session_id: Option<&str>) -> Result<String, AgentError> {
    let safe = |raw: &str| -> String {
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    if let Some(pane) = tmux_pane.filter(|pane| !pane.is_empty()) {
        return Ok(format!("pane-{}", safe(pane.trim_start_matches('%'))));
    }
    if let Some(id) = session_id.filter(|id| !id.is_empty()) {
        return Ok(format!("session-{}", safe(id)));
    }
    Err(AgentError::NoIdentity)
}

/// Collapse whitespace and cap length so one huge prompt cannot bloat state.
fn clip(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TEXT_LIMIT {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(TEXT_LIMIT - 1).collect();
    out.push('…');
    out
}

fn write_record(dir: &Path, record: &AgentRecord) -> Result<(), AgentError> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| AgentError::Io { path, source }
    };
    fs::create_dir_all(dir).map_err(io_err(dir))?;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    let final_path = dir.join(format!("{}.json", record.key));
    let temp = dir.join(format!(".{}.{}.tmp", record.key, std::process::id()));
    let body = serde_json::to_vec_pretty(record).map_err(|error| AgentError::Io {
        path: final_path.clone(),
        source: io::Error::other(error),
    })?;
    fs::write(&temp, body).map_err(io_err(&temp))?;
    fs::rename(&temp, &final_path).map_err(io_err(&final_path))
}

fn read_record(path: &Path) -> Option<AgentRecord> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_dir_records(dir: &Path) -> Result<Vec<(PathBuf, AgentRecord)>, AgentError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AgentError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    let mut records: Vec<(PathBuf, AgentRecord)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .filter_map(|path| read_record(&path).map(|record| (path, record)))
        .collect();
    records.sort_by(|a, b| {
        (a.1.tmux_session.as_deref(), &a.1.key).cmp(&(b.1.tmux_session.as_deref(), &b.1.key))
    });
    Ok(records)
}

fn remove(path: &Path) -> Result<(), AgentError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AgentError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Live tmux panes on this machine's default tmux server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneSnapshot {
    Live(BTreeSet<String>),
    /// tmux is installed but no server is running: every pane is gone.
    NoServer,
    /// tmux could not be asked; keep reported states.
    Unknown,
}

pub fn tmux_panes() -> PaneSnapshot {
    let mut cmd = Command::new("tmux");
    cmd.args(["list-panes", "-a", "-F", "#{pane_id}"]);
    match run_with_deadline(
        &mut cmd,
        TMUX_TIMEOUT,
        Duration::from_secs(1),
        Duration::from_millis(10),
    ) {
        Ok(output) if output.code == 0 => PaneSnapshot::Live(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        Ok(output) if !output.timed_out => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("error connecting") {
                PaneSnapshot::NoServer
            } else {
                PaneSnapshot::Unknown
            }
        }
        _ => PaneSnapshot::Unknown,
    }
}

fn tmux_session_of_pane(pane: &str) -> Option<String> {
    let mut cmd = Command::new("tmux");
    cmd.args(["display-message", "-p", "-t", pane, "#{session_name}"]);
    let output = run_with_deadline(
        &mut cmd,
        TMUX_TIMEOUT,
        Duration::from_secs(1),
        Duration::from_millis(10),
    )
    .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.code == 0 && !name.is_empty()).then_some(name)
}

/// This machine's agent records with liveness applied. Records whose pane is
/// gone are reported as [`AgentState::Gone`] and deleted once stale.
pub fn local_agents(
    dir: &Path,
    panes: &PaneSnapshot,
    now: u64,
) -> Result<Vec<AgentRecord>, AgentError> {
    let mut out = Vec::new();
    for (path, mut record) in read_dir_records(dir)? {
        if let Some(pane) = &record.tmux_pane {
            let alive = match panes {
                PaneSnapshot::Live(live) => live.contains(pane),
                PaneSnapshot::NoServer => false,
                PaneSnapshot::Unknown => true,
            };
            if !alive {
                if now.saturating_sub(record.updated_at) > GONE_RETENTION_SECS {
                    remove(&path)?;
                    continue;
                }
                record.state = AgentState::Gone;
            }
        }
        out.push(record);
    }
    Ok(out)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// `45s`, `12m`, `3h`, `2d`.
pub fn age_label(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Resume command for a moved session: the recorded one, or the known form
/// for Claude Code and Codex.
pub fn resume_command(record: &AgentRecord) -> Option<String> {
    if let Some(command) = &record.resume_command {
        return Some(command.clone());
    }
    let id = record.session_id.as_deref()?;
    match record.agent.as_str() {
        "claude" | "claude-code" => Some(format!("claude --resume {id}")),
        "codex" => Some(format!("codex resume {id}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fleet-agents-{name}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parses_set_and_clear() {
        let parsed = parse_hook_args(&args(
            "set --state needs-input --agent claude --from-hook-json",
        ))
        .unwrap();
        assert!(parsed.from_hook_json);
        match parsed.command {
            HookCommand::Set { state, fields } => {
                assert_eq!(state, AgentState::NeedsInput);
                assert_eq!(fields.agent.as_deref(), Some("claude"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            parse_hook_args(&args("clear --session-id abc"))
                .unwrap()
                .command,
            HookCommand::Clear {
                session_id: Some(_)
            }
        ));
        assert!(parse_hook_args(&args("set")).is_err());
        assert!(parse_hook_args(&args("set --state sleeping")).is_err());
        assert!(parse_hook_args(&args("set --state done --bogus x")).is_err());
        assert!(parse_hook_args(&args("set --state done --task")).is_err());
    }

    #[test]
    fn payload_fills_unset_fields_only() {
        let mut fields = HookFields {
            task: Some("explicit".into()),
            ..HookFields::default()
        };
        merge_hook_payload(
            &mut fields,
            Some(AgentState::Running),
            r#"{"session_id":"s1","transcript_path":"/t.jsonl","cwd":"/repo","prompt":"from payload"}"#,
        );
        assert_eq!(fields.session_id.as_deref(), Some("s1"));
        assert_eq!(fields.task.as_deref(), Some("explicit"));
        let mut fields = HookFields::default();
        merge_hook_payload(
            &mut fields,
            Some(AgentState::NeedsInput),
            r#"{"tool_name":"Bash"}"#,
        );
        assert_eq!(fields.request.as_deref(), Some("permission: Bash"));
        let mut fields = HookFields::default();
        merge_hook_payload(&mut fields, Some(AgentState::Done), "not json");
        assert_eq!(fields, HookFields::default());
    }

    #[test]
    fn set_keeps_task_and_clears_request_across_states() {
        let dir = temp_dir("set");
        let fields = |task: Option<&str>, request: Option<&str>| HookFields {
            agent: Some("claude".into()),
            session_id: Some("s1".into()),
            task: task.map(str::to_string),
            request: request.map(str::to_string),
            cwd: Some("/repo".into()),
            ..HookFields::default()
        };
        set_record(
            &dir,
            AgentState::Running,
            fields(Some("fix   the\nbuild"), None),
            Some("%3"),
            Some("main".into()),
            100,
        )
        .unwrap();
        let waiting = set_record(
            &dir,
            AgentState::NeedsInput,
            fields(None, Some("permission: Bash")),
            Some("%3"),
            None,
            110,
        )
        .unwrap();
        assert_eq!(waiting.key, "pane-3");
        assert_eq!(waiting.task.as_deref(), Some("fix the build"));
        assert_eq!(waiting.tmux_session.as_deref(), Some("main"));
        assert_eq!(waiting.detail(), "permission: Bash");
        let running = set_record(
            &dir,
            AgentState::Running,
            fields(None, None),
            Some("%3"),
            None,
            120,
        )
        .unwrap();
        assert_eq!(running.request, None);

        let mut other = fields(Some("new task"), None);
        other.session_id = Some("s2".into());
        let fresh = set_record(&dir, AgentState::Running, other, Some("%3"), None, 130).unwrap();
        assert_eq!(fresh.task.as_deref(), Some("new task"));
        assert_eq!(
            fresh.tmux_session, None,
            "a new session in the pane starts fresh"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_marks_gone_panes_and_prunes_stale_ones() {
        let dir = temp_dir("list");
        let base = HookFields {
            agent: Some("codex".into()),
            ..HookFields::default()
        };
        set_record(
            &dir,
            AgentState::Running,
            base.clone(),
            Some("%1"),
            None,
            1_000,
        )
        .unwrap();
        set_record(
            &dir,
            AgentState::Done,
            base.clone(),
            Some("%2"),
            None,
            1_000,
        )
        .unwrap();
        let mut detached = base;
        detached.session_id = Some("outside-tmux".into());
        set_record(&dir, AgentState::Idle, detached, None, None, 1_000).unwrap();

        let live = PaneSnapshot::Live(["%1".to_string()].into_iter().collect());
        let records = local_agents(&dir, &live, 2_000).unwrap();
        let states: Vec<_> = records.iter().map(|r| (r.key.as_str(), r.state)).collect();
        assert!(states.contains(&("pane-1", AgentState::Running)));
        assert!(states.contains(&("pane-2", AgentState::Gone)));
        assert!(states.contains(&("session-outside-tmux", AgentState::Idle)));

        let later = 1_000 + GONE_RETENTION_SECS + 1;
        let records = local_agents(&dir, &PaneSnapshot::NoServer, later).unwrap();
        assert_eq!(records.len(), 1, "only the non-tmux record survives");
        assert!(!dir.join("pane-2.json").exists());

        clear_records(&dir, None, Some("outside-tmux")).unwrap();
        assert!(local_agents(&dir, &PaneSnapshot::Unknown, later)
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_is_required() {
        let dir = temp_dir("identity");
        assert!(matches!(
            set_record(&dir, AgentState::Done, HookFields::default(), None, None, 1),
            Err(AgentError::NoIdentity)
        ));
        assert!(matches!(
            clear_records(&dir, None, None),
            Err(AgentError::NoIdentity)
        ));
    }

    #[test]
    fn resume_commands_for_known_agents() {
        let mut record = AgentRecord {
            version: 1,
            key: "pane-1".into(),
            agent: "claude".into(),
            state: AgentState::Done,
            task: None,
            request: None,
            message: None,
            session_id: Some("abc".into()),
            transcript: None,
            resume_command: None,
            cwd: None,
            tmux_session: None,
            tmux_pane: None,
            updated_at: 0,
        };
        assert_eq!(
            resume_command(&record).as_deref(),
            Some("claude --resume abc")
        );
        record.agent = "codex".into();
        assert_eq!(resume_command(&record).as_deref(), Some("codex resume abc"));
        record.agent = "pi".into();
        assert_eq!(resume_command(&record), None);
        record.resume_command = Some("pi --continue".into());
        assert_eq!(resume_command(&record).as_deref(), Some("pi --continue"));
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(age_label(100, 55), "45s");
        assert_eq!(age_label(10_000, 9_000), "16m");
        assert_eq!(age_label(100_000, 0), "1d");
    }
}
