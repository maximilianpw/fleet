//! `fleet status` and `fleet agents`: one query per host, run concurrently.
//!
//! A remote host answers one `sh` probe that prints its Fleet version, tmux
//! sessions, and `fleet agents --local --json`. The current machine is read
//! in-process so it reports this binary's own records and version.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::agents::{age_label, local_agents, tmux_panes, AgentEnv, AgentRecord, AgentState};
use crate::config::{FleetConfig, ResolvedHost, DEFAULT_TMUX_COMMAND};
use crate::doctor::{classify_ssh_stderr, DoctorEvent, SshState};
use crate::process::run_with_deadline;
use crate::remote::{fan_out, Endpoint, QueryOutput, Runner, REMOTE_PATH_SETUP};
use crate::tailscale::Presence;

pub const FLEET_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TmuxSession {
    pub name: String,
    pub attached: bool,
    pub windows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    Local,
    Reachable,
    /// Tailscale says the host is offline, so it was not contacted.
    Skipped,
    Auth,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetPresence {
    Version(String),
    Missing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TmuxPresence {
    Ok,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostSnapshot {
    pub host: String,
    pub presence: Presence,
    pub reach: Reach,
    pub fleet: FleetPresence,
    pub tmux: TmuxPresence,
    pub sessions: Vec<TmuxSession>,
    /// `None` when agent state could not be read (unreachable, or no Fleet).
    pub agents: Option<Vec<AgentRecord>>,
}

/// Probe printed by remote hosts. Section markers keep parsing independent
/// of what each tool prints.
pub fn remote_probe_script(tmux_command: &str) -> String {
    format!(
        ": fleet-probe; {REMOTE_PATH_SETUP}; \
echo @fleet; if command -v fleet >/dev/null 2>&1; then fleet --version 2>/dev/null; else echo missing; fi; \
echo @tmux; if command -v {tmux_command} >/dev/null 2>&1; then echo ok; {tmux_command} list-sessions -F \"#{{session_name}}|#{{session_attached}}|#{{session_windows}}\" 2>/dev/null; else echo missing; fi; \
echo @agents; if command -v fleet >/dev/null 2>&1; then fleet agents --local --json 2>/dev/null; fi; \
echo @end"
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeSections {
    pub fleet: Vec<String>,
    pub tmux: Vec<String>,
    pub agents: String,
    pub complete: bool,
}

pub fn parse_probe(stdout: &str) -> ProbeSections {
    let mut sections = ProbeSections::default();
    let mut current = "";
    for line in stdout.lines() {
        match line.trim() {
            "@fleet" | "@tmux" | "@agents" => {
                current = line.trim();
                continue;
            }
            "@end" => {
                sections.complete = true;
                break;
            }
            _ => {}
        }
        match current {
            "@fleet" if !line.trim().is_empty() => sections.fleet.push(line.trim().to_string()),
            "@tmux" if !line.trim().is_empty() => sections.tmux.push(line.trim().to_string()),
            "@agents" => {
                sections.agents.push_str(line);
                sections.agents.push('\n');
            }
            _ => {}
        }
    }
    sections
}

/// `fleet 0.1.0` -> `0.1.0`.
fn parse_fleet_version(lines: &[String]) -> FleetPresence {
    match lines.first().map(String::as_str) {
        Some("missing") => FleetPresence::Missing,
        Some(line) => FleetPresence::Version(
            line.strip_prefix("fleet ")
                .unwrap_or(line)
                .trim()
                .to_string(),
        ),
        None => FleetPresence::Unknown,
    }
}

pub fn parse_session_line(line: &str) -> Option<TmuxSession> {
    let mut parts = line.splitn(3, '|');
    let name = parts.next()?.to_string();
    let attached = parts.next()?.trim().parse::<u32>().ok()? > 0;
    let windows = parts.next()?.trim().parse().ok()?;
    (!name.is_empty()).then_some(TmuxSession {
        name,
        attached,
        windows,
    })
}

fn snapshot_from_probe(host: &str, presence: Presence, output: &QueryOutput) -> HostSnapshot {
    let sections = parse_probe(&output.stdout);
    if !output.success() && !sections.complete {
        let reach = match classify_ssh_stderr(&output.stderr) {
            SshState::Auth => Reach::Auth,
            _ => Reach::Unavailable,
        };
        return HostSnapshot {
            host: host.to_string(),
            presence,
            reach,
            fleet: FleetPresence::Unknown,
            tmux: TmuxPresence::Unknown,
            sessions: Vec::new(),
            agents: None,
        };
    }
    let (tmux, sessions) = match sections.tmux.split_first() {
        Some((first, rest)) if first == "ok" => (
            TmuxPresence::Ok,
            rest.iter()
                .filter_map(|line| parse_session_line(line))
                .collect(),
        ),
        Some((first, _)) if first == "missing" => (TmuxPresence::Missing, Vec::new()),
        _ => (TmuxPresence::Unknown, Vec::new()),
    };
    let fleet = parse_fleet_version(&sections.fleet);
    let agents = if sections.agents.trim().is_empty() {
        None
    } else {
        serde_json::from_str::<Vec<AgentRecord>>(&sections.agents).ok()
    };
    HostSnapshot {
        host: host.to_string(),
        presence,
        reach: Reach::Reachable,
        fleet,
        tmux,
        sessions,
        agents,
    }
}

fn local_sessions() -> (TmuxPresence, Vec<TmuxSession>) {
    let mut cmd = Command::new(DEFAULT_TMUX_COMMAND);
    cmd.args([
        "list-sessions",
        "-F",
        "#{session_name}|#{session_attached}|#{session_windows}",
    ]);
    match run_with_deadline(
        &mut cmd,
        Duration::from_secs(3),
        Duration::from_secs(1),
        Duration::from_millis(10),
    ) {
        Ok(output) => (
            TmuxPresence::Ok,
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(parse_session_line)
                .collect(),
        ),
        Err(_) => (TmuxPresence::Missing, Vec::new()),
    }
}

fn local_snapshot(host: &str, presence: Presence, agent_env: &AgentEnv) -> HostSnapshot {
    let (tmux, sessions) = local_sessions();
    let agents = agent_env
        .state_dir()
        .ok()
        .and_then(|dir| local_agents(&dir, &tmux_panes(), crate::agents::now_secs()).ok());
    HostSnapshot {
        host: host.to_string(),
        presence,
        reach: Reach::Local,
        fleet: FleetPresence::Version(FLEET_VERSION.to_string()),
        tmux,
        sessions,
        agents,
    }
}

/// Snapshot `hosts` (canonical names) concurrently. Hosts Tailscale reports
/// offline are skipped instead of waiting for an SSH timeout.
pub fn gather(
    config: &FleetConfig,
    hosts: &[String],
    presence: &BTreeMap<String, Presence>,
    runner: &Runner,
    agent_env: &AgentEnv,
) -> Vec<HostSnapshot> {
    fan_out(hosts, |name| {
        let presence = presence.get(name).cloned().unwrap_or(Presence::Unknown);
        let Some(resolved) = config.resolve(name) else {
            return HostSnapshot {
                host: name.clone(),
                presence,
                reach: Reach::Unavailable,
                fleet: FleetPresence::Unknown,
                tmux: TmuxPresence::Unknown,
                sessions: Vec::new(),
                agents: None,
            };
        };
        if resolved.is_local {
            return local_snapshot(name, presence, agent_env);
        }
        if matches!(presence, Presence::Offline { .. }) {
            return HostSnapshot {
                host: name.clone(),
                presence,
                reach: Reach::Skipped,
                fleet: FleetPresence::Unknown,
                tmux: TmuxPresence::Unknown,
                sessions: Vec::new(),
                agents: None,
            };
        }
        let output = runner.run(
            &Endpoint::Ssh {
                target: resolved.ssh_target.clone(),
            },
            &remote_probe_script(&resolved.tmux_command),
        );
        snapshot_from_probe(name, presence, &output)
    })
}

/// Doctor's tool checks: tmux must exist; a missing or different `fleet` is a
/// warning, because only `agents` and `move` need it remotely.
pub fn doctor_tool_events(resolved: &ResolvedHost, runner: &Runner) -> Vec<DoctorEvent> {
    let (tmux, fleet, command) = if resolved.is_local {
        (
            local_sessions().0,
            FleetPresence::Version(FLEET_VERSION.to_string()),
            DEFAULT_TMUX_COMMAND.to_string(),
        )
    } else {
        let output = runner.run(
            &Endpoint::Ssh {
                target: resolved.ssh_target.clone(),
            },
            &remote_probe_script(&resolved.tmux_command),
        );
        let snapshot = snapshot_from_probe(&resolved.canonical, Presence::Unknown, &output);
        if snapshot.reach != Reach::Reachable {
            return vec![DoctorEvent::Warning {
                text: format!(
                    "could not check tmux and fleet on {}: {}",
                    resolved.canonical,
                    output.stderr.trim()
                ),
            }];
        }
        (snapshot.tmux, snapshot.fleet, resolved.tmux_command.clone())
    };
    let mut events = vec![DoctorEvent::Tmux {
        command: command.clone(),
        available: match tmux {
            TmuxPresence::Ok => Some(true),
            TmuxPresence::Missing => Some(false),
            TmuxPresence::Unknown => None,
        },
    }];
    if tmux == TmuxPresence::Missing {
        events.push(DoctorEvent::Unhealthy {
            text: format!(
                "tmux command {command} is not available on {}",
                resolved.canonical
            ),
        });
    }
    if resolved.is_local {
        return events;
    }
    match fleet {
        FleetPresence::Version(version) => {
            let differs = version != FLEET_VERSION;
            events.push(DoctorEvent::Fleet {
                version: Some(version.clone()),
            });
            if differs {
                events.push(DoctorEvent::Warning {
                    text: format!(
                        "{} runs fleet {version}; this machine runs {FLEET_VERSION}",
                        resolved.canonical
                    ),
                });
            }
        }
        FleetPresence::Missing => {
            events.push(DoctorEvent::Fleet { version: None });
            events.push(DoctorEvent::Warning {
                text: format!(
                    "fleet is not on the non-interactive SSH PATH of {}; agent status and move need it",
                    resolved.canonical
                ),
            });
        }
        FleetPresence::Unknown => {}
    }
    events
}

fn agent_summary(snapshot: &HostSnapshot) -> String {
    let Some(agents) = &snapshot.agents else {
        return match (&snapshot.reach, &snapshot.fleet) {
            (Reach::Reachable, FleetPresence::Missing) => "no fleet".into(),
            (Reach::Reachable | Reach::Local, _) => "-".into(),
            _ => "?".into(),
        };
    };
    let order = [
        AgentState::NeedsInput,
        AgentState::Running,
        AgentState::Done,
        AgentState::Idle,
        AgentState::Gone,
    ];
    let parts: Vec<String> = order
        .iter()
        .filter_map(|state| {
            let count = agents.iter().filter(|agent| agent.state == *state).count();
            (count > 0).then(|| format!("{} {count}", state.label()))
        })
        .collect();
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(", ")
    }
}

fn reach_label(snapshot: &HostSnapshot) -> Option<&'static str> {
    match snapshot.reach {
        Reach::Local | Reach::Reachable => None,
        Reach::Skipped => Some("offline"),
        Reach::Auth => Some("ssh auth failure"),
        Reach::Unavailable => Some("unreachable"),
    }
}

pub fn render_status(current_host: &str, snapshots: &[HostSnapshot], now: u64) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Current machine: {current_host}\n");
    let _ = writeln!(
        out,
        "{:<18} {:<10} {:<18} {:<9} {:<28} AGENTS",
        "HOST", "NETWORK", "PATH", "FLEET", "SESSIONS"
    );
    for snapshot in snapshots {
        let path = match &snapshot.presence {
            Presence::Offline {
                last_seen: Some(seen),
            } => format!("seen {seen}"),
            other => other.path_label(),
        };
        let fleet = match &snapshot.fleet {
            FleetPresence::Version(version) => version.clone(),
            FleetPresence::Missing => "missing".into(),
            FleetPresence::Unknown => "-".into(),
        };
        let sessions = match (reach_label(snapshot), &snapshot.tmux) {
            (Some(label), _) => label.to_string(),
            (None, TmuxPresence::Missing) => "tmux missing".into(),
            (None, _) if snapshot.sessions.is_empty() => "-".into(),
            (None, _) => snapshot
                .sessions
                .iter()
                .map(|session| {
                    if session.attached {
                        format!("{}*", session.name)
                    } else {
                        session.name.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
        };
        let _ = writeln!(
            out,
            "{:<18} {:<10} {:<18} {:<9} {:<28} {}",
            snapshot.host,
            network_label(&snapshot.presence),
            path,
            fleet,
            sessions,
            agent_summary(snapshot)
        );
    }
    let agents = render_agent_rows(snapshots, now);
    if !agents.is_empty() {
        let _ = write!(out, "\n{agents}");
    }
    out
}

fn network_label(presence: &Presence) -> String {
    match presence {
        Presence::Offline { .. } => "offline".into(),
        other => other.network_label(),
    }
}

/// Agent table, or an empty string when no host reported agents.
pub fn render_agent_rows(snapshots: &[HostSnapshot], now: u64) -> String {
    let rows: Vec<(&str, &AgentRecord)> = snapshots
        .iter()
        .flat_map(|snapshot| {
            snapshot
                .agents
                .iter()
                .flatten()
                .map(move |agent| (snapshot.host.as_str(), agent))
        })
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<18} {:<14} {:<13} {:<8} {:<5} DETAIL",
        "HOST", "SESSION", "STATE", "AGENT", "AGE"
    );
    for (host, agent) in rows {
        let _ = writeln!(
            out,
            "{:<18} {:<14} {:<13} {:<8} {:<5} {}",
            host,
            agent.tmux_session.as_deref().unwrap_or("-"),
            agent.state.label(),
            agent.agent,
            age_label(now, agent.updated_at),
            agent.detail()
        );
    }
    out
}

pub fn render_agents(snapshots: &[HostSnapshot], now: u64) -> String {
    let mut out = render_agent_rows(snapshots, now);
    if out.is_empty() {
        out.push_str("No agents reported.\n");
    }
    for snapshot in snapshots {
        if snapshot.agents.is_some() {
            continue;
        }
        let reason = match (&snapshot.reach, &snapshot.fleet) {
            (Reach::Skipped, _) => "offline".to_string(),
            (Reach::Auth, _) => "SSH authentication failed".to_string(),
            (Reach::Unavailable, _) => "unreachable".to_string(),
            (_, FleetPresence::Missing) => "fleet is not on the remote PATH".to_string(),
            _ => "agent state unavailable".to_string(),
        };
        let _ = writeln!(out, "{}: {reason}", snapshot.host);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_complete_probe() {
        let stdout = "@fleet\nfleet 0.1.0\n@tmux\nok\nmain|1|3\nagents|0|1\n@agents\n[]\n@end\n";
        let output = QueryOutput {
            code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
        };
        let snapshot = snapshot_from_probe("workbox", Presence::Unknown, &output);
        assert_eq!(snapshot.reach, Reach::Reachable);
        assert_eq!(snapshot.fleet, FleetPresence::Version("0.1.0".into()));
        assert_eq!(snapshot.tmux, TmuxPresence::Ok);
        assert_eq!(snapshot.sessions.len(), 2);
        assert!(snapshot.sessions[0].attached);
        assert_eq!(snapshot.agents, Some(Vec::new()));
    }

    #[test]
    fn missing_tools_and_failures() {
        let output = QueryOutput {
            code: 0,
            stdout: "@fleet\nmissing\n@tmux\nmissing\n@agents\n@end\n".into(),
            stderr: String::new(),
            timed_out: false,
        };
        let snapshot = snapshot_from_probe("box", Presence::Unknown, &output);
        assert_eq!(snapshot.fleet, FleetPresence::Missing);
        assert_eq!(snapshot.tmux, TmuxPresence::Missing);
        assert_eq!(snapshot.agents, None);
        assert_eq!(agent_summary(&snapshot), "no fleet");

        let output = QueryOutput {
            code: 255,
            stdout: String::new(),
            stderr: "Permission denied (publickey).".into(),
            timed_out: false,
        };
        assert_eq!(
            snapshot_from_probe("box", Presence::Unknown, &output).reach,
            Reach::Auth
        );
    }

    #[test]
    fn probe_script_has_no_backslashes() {
        assert!(!remote_probe_script("/run/current-system/sw/bin/tmux").contains('\\'));
    }
}
