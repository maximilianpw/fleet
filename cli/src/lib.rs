//! Fleet configuration, SSH/tmux argument construction, and ad-hoc forwards.
//!
//! The `fleet` binary loads TOML, validates it, and execs OpenSSH or tmux.
//! Tunnel supervision, doctor, and the runner live in sibling modules.

use std::io::{self, Write};
use std::str::FromStr;

pub mod agents;
pub mod config;
pub mod copy;
pub mod doctor;
pub mod forwards;
pub mod launchd;
pub mod moving;
pub mod ports;
pub mod process;
pub mod remote;
pub mod runner;
pub mod select;
pub mod ssh;
pub mod status;
pub mod tailscale;
pub mod tunnels;

pub use config::{
    load_config, parse_config, parse_port, resolve_config_origin, ConfigError, ConfigOrigin,
    FleetConfig, HostConfig, Port, PortError, ResolvedHost, Supervisor, TargetError, TunnelMapping,
    TunnelsConfig, SCHEMA_VERSION,
};
pub use copy::{plan_copy, plan_copy_with, CopyError, CopyOptions};
pub use forwards::{
    collect_forward_rows, ensure_ports_free, forward_local_port, parse_forward_pids,
    render_forward_list, stop_forwards, ConservativeManagedInspector, ForwardError, ForwardRow,
    ManagedClass, ManagedForwardInspector,
};
pub use process::{
    exec_replace, parse_ps_output, PathKill, PathPsTable, PlannedCommand, ProcessEnv, ProcessError,
    ProcessTable, SignalSender,
};
pub use ssh::{
    local_ports_of, parse_ssh_tail, plan_ad_hoc_forward, plan_run, plan_shell, plan_ssh, plan_t3,
    AttachmentForward, SshError,
};

/// Help text preserved from the legacy CLI, plus the commands added since.
pub const USAGE: &str = "\
usage:
  fleet list [--json]
  fleet status [HOST...] [--where EXPR] [--json]
  fleet agents [HOST...] [--where EXPR] [--local] [--json]
  fleet pick [--where EXPR] [--all]
  fleet ssh HOST [SESSION] [--forward PORT|LOCAL_PORT:REMOTE_PORT]...
  fleet shell HOST
  fleet run HOST COMMAND...
  fleet copy [-r] SOURCE DESTINATION
  fleet ports HOST [--json]
  fleet forward HOST LOCAL_PORT REMOTE_PORT [REMOTE_HOST]
  fleet forward list [--json] [LOCAL_PORT]
  fleet forward stop PID...
  fleet forward delete PID...
  fleet t3 HOST [LOCAL_PORT]
  fleet tunnel status [--json]
  fleet tunnel pause PORT
  fleet tunnel resume PORT
  fleet doctor [HOST] [--where EXPR] [--json]
  fleet move HOST SESSION TARGET [--keep] [--force]
  fleet hook set --state STATE [OPTIONS]
  fleet hook clear [--session-id ID]
  fleet config validate
  fleet completions SHELL

examples:
  fleet ssh workbox
  fleet ssh workbox agents --forward 3000 --forward 5173
  fleet shell workbox
  fleet run workbox btop
  fleet copy report.md workbox
  fleet copy workbox:/tmp/report.md .
  fleet forward workbox 3000 3000
  fleet forward list 3000
  fleet forward delete 12345
  fleet t3 workbox 51001
  fleet tunnel status
  fleet tunnel pause 3000
  fleet tunnel resume 3000
  fleet doctor workbox
  fleet status --where long_running_agents
  fleet run \"$(fleet pick --where '!local,long_running_agents')\" make test
  fleet move workbox agents laptop";

#[derive(Debug, thiserror::Error)]
pub enum FleetError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Ssh(#[from] SshError),
    #[error(transparent)]
    Copy(#[from] CopyError),
    #[error(transparent)]
    Forward(#[from] ForwardError),
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Target(#[from] TargetError),
    #[error(transparent)]
    Tunnel(#[from] tunnels::TunnelError),
    #[error(transparent)]
    Doctor(#[from] doctor::DoctorError),
    #[error(transparent)]
    Agent(#[from] agents::AgentError),
    #[error(transparent)]
    Filter(#[from] select::FilterError),
    #[error(transparent)]
    Remote(#[from] remote::RemoteError),
    #[error(transparent)]
    Move(#[from] moving::MoveError),
    #[error("fleet: no host matches --where {0}")]
    NoMatch(String),
    #[error("{0}")]
    Usage(String),
    #[error("fleet: failed to write output: {0}")]
    Output(#[from] io::Error),
}

impl FleetError {
    pub fn usage() -> Self {
        Self::Usage(USAGE.to_string())
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Config(error) => error.exit_code(),
            Self::Ssh(error) => error.exit_code(),
            Self::Copy(error) => error.exit_code(),
            Self::Forward(error) => error.exit_code(),
            Self::Process(error) => error.exit_code(),
            Self::Port(error) => error.exit_code(),
            Self::Target(error) => error.exit_code(),
            Self::Tunnel(error) => error.exit_code(),
            Self::Doctor(error) => error.exit_code(),
            Self::Agent(error) => error.exit_code(),
            Self::Filter(error) => error.exit_code(),
            Self::Remote(error) => error.exit_code(),
            Self::Move(error) => error.exit_code(),
            Self::NoMatch(_) => 1,
            Self::Usage(_) => 2,
            Self::Output(_) => 1,
        }
    }
}

/// Write command output without panicking when stdout is closed.
pub fn write_stdout(output: impl AsRef<[u8]>) -> Result<(), FleetError> {
    let mut out = io::stdout().lock();
    out.write_all(output.as_ref())?;
    out.flush()?;
    Ok(())
}

/// True for a non-empty run of ASCII digits. Rejects signs and whitespace
/// that `str::parse` would otherwise accept or report differently.
pub(crate) fn is_decimal(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Parse an unsigned decimal token; `None` for non-digits or overflow.
pub(crate) fn parse_decimal<T: FromStr>(text: &str) -> Option<T> {
    if is_decimal(text) {
        text.parse().ok()
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelCommand {
    Status { json: bool },
    Pause { port: u16 },
    Resume { port: u16 },
}

/// Runtime mappings with hosts as written in the config. `tunnel status`
/// displays these names.
pub fn mappings_from_config(config: &FleetConfig) -> Vec<tunnels::Mapping> {
    config
        .tunnels
        .mappings
        .iter()
        .map(tunnels::Mapping::from)
        .collect()
}

/// Runtime mappings with alias hosts replaced by their canonical name, so
/// `doctor HOST` finds every mapping that targets HOST.
pub fn doctor_mappings_from_config(config: &FleetConfig) -> Vec<tunnels::Mapping> {
    let mut mappings = mappings_from_config(config);
    for mapping in &mut mappings {
        if let Some(resolved) = config.resolve(&mapping.host) {
            mapping.host = resolved.canonical;
        }
    }
    mappings
}

fn tunnel_context<'a>(
    config: &'a FleetConfig,
    env: &'a ProcessEnv,
    mappings: &'a [tunnels::Mapping],
) -> Result<tunnels::TunnelContext<'a>, FleetError> {
    let home = env.home.as_deref().ok_or(ConfigError::HomeNotSet)?;
    Ok(tunnels::TunnelContext {
        supervisor: config.tunnels.supervisor,
        mappings,
        home,
        uid: launchd::current_uid(),
    })
}

/// Public CLI hook for `fleet tunnel ...`.
pub fn run_tunnel_command(
    config: &FleetConfig,
    command: TunnelCommand,
    env: &ProcessEnv,
) -> Result<(), FleetError> {
    let mappings = mappings_from_config(config);
    let ctx = tunnel_context(config, env, &mappings)?;
    let launchd = launchd::SystemLaunchctl::new();
    let listen = tunnels::SystemListenProbe::new();
    match command {
        TunnelCommand::Status { json: false } => {
            tunnels::print_status(&mut io::stdout().lock(), &ctx, &launchd, &listen)?;
            Ok(())
        }
        TunnelCommand::Status { json: true } => {
            write_json(&tunnels::status_rows(&ctx, &launchd, &listen)?)
        }
        TunnelCommand::Pause { port } => write_stdout(tunnels::pause(port, &ctx, &launchd)?),
        TunnelCommand::Resume { port } => {
            write_stdout(tunnels::resume(port, &ctx, &launchd, &listen)?)
        }
    }
}

/// Public CLI hook for `fleet doctor [HOST]`.
///
/// `hosts` holds canonical names or aliases the CLI already validated.
/// `explicit_host` is true for `doctor HOST`, which keeps the historical
/// unheaded text and a single JSON object. Otherwise every report is headed
/// by `== HOST ==` and JSON is an array, even when one host is selected.
/// Hosts are checked concurrently. After the historical checks, a reachable host also
/// reports whether its tmux command and `fleet` are available.
pub fn run_doctor_command(
    config: &FleetConfig,
    hosts: &[String],
    env: &ProcessEnv,
    json: bool,
    explicit_host: bool,
) -> Result<(), FleetError> {
    let mappings = doctor_mappings_from_config(config);
    let ctx = tunnel_context(config, env, &mappings)?;
    let launchd = launchd::SystemLaunchctl::new();
    let listen = tunnels::SystemListenProbe::new();
    let ssh = doctor::SystemSsh::new();
    let runner = remote::Runner::new(env);
    let reports = remote::fan_out(hosts, |token| {
        let resolved = config
            .resolve(token)
            .ok_or_else(|| SshError::UnknownHost(token.clone()))?;
        let doctor_host = doctor::DoctorHost {
            canonical: resolved.canonical.clone(),
            ssh_target: resolved.ssh_target.clone(),
            is_local: resolved.is_local,
        };
        let mut report = doctor::doctor_report(
            &doctor_host,
            &ctx,
            &launchd,
            &listen,
            &ssh,
            doctor::DoctorDeadlines::production(),
        );
        let reachable = report.events.iter().any(|event| {
            matches!(
                event,
                doctor::DoctorEvent::Ssh {
                    state: doctor::SshState::Reachable | doctor::SshState::Skipped
                }
            )
        });
        if report.stopped.is_none() && reachable {
            report
                .events
                .extend(status::doctor_tool_events(&resolved, &runner));
        }
        Ok::<_, SshError>((resolved.canonical, report))
    });
    let reports = reports.into_iter().collect::<Result<Vec<_>, _>>()?;

    if json {
        let rendered: Vec<_> = reports
            .iter()
            .map(|(host, report)| {
                serde_json::json!({
                    "host": host,
                    "healthy": report.healthy(),
                    "events": report.events,
                    "error": report.stopped.as_ref().map(ToString::to_string),
                })
            })
            .collect();
        let value = match rendered.as_slice() {
            [single] if explicit_host => single.clone(),
            _ => serde_json::Value::Array(rendered),
        };
        write_json(&value)?;
    } else if explicit_host {
        write_stdout(reports[0].1.render())?;
    } else {
        let text: Vec<String> = reports
            .iter()
            .map(|(host, report)| format!("== {host} ==\n{}", report.render()))
            .collect();
        write_stdout(text.join("\n"))?;
    }

    let mut unhealthy = false;
    for (_, report) in reports {
        if let Some(error) = report.stopped {
            return Err(error.into());
        }
        unhealthy |= !report.healthy();
    }
    if unhealthy {
        Err(doctor::DoctorError::Unhealthy.into())
    } else {
        Ok(())
    }
}

/// Write `value` as pretty JSON plus a newline.
pub fn write_json(value: &impl serde::Serialize) -> Result<(), FleetError> {
    let mut text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    text.push('\n');
    write_stdout(text)
}

/// Canonical hosts selected by explicit names or aliases, else every host,
/// then narrowed by `filter`. Presence is read only when needed.
pub fn select_hosts(
    config: &FleetConfig,
    names: &[String],
    filter: Option<&select::HostFilter>,
    presence: &std::collections::BTreeMap<String, tailscale::Presence>,
) -> Result<Vec<String>, FleetError> {
    let candidates: Vec<String> = if names.is_empty() {
        config.hosts.keys().cloned().collect()
    } else {
        let mut out = Vec::new();
        for name in names {
            let resolved = config
                .resolve(name)
                .ok_or_else(|| SshError::UnknownHost(name.clone()))?;
            if !out.contains(&resolved.canonical) {
                out.push(resolved.canonical);
            }
        }
        out
    };
    let Some(filter) = filter else {
        return Ok(candidates);
    };
    let mut selected = Vec::new();
    for name in candidates {
        let host = &config.hosts[&name];
        let online = presence.get(&name).and_then(tailscale::Presence::online);
        if filter.matches(config, &name, host, online)? {
            selected.push(name);
        }
    }
    Ok(selected)
}

fn presence_map(config: &FleetConfig) -> std::collections::BTreeMap<String, tailscale::Presence> {
    tailscale::presence_for(config, tailscale::read_status().as_ref())
}

/// Read Tailscale presence only when `filter` asks for `online`.
pub fn presence_for_filter(
    config: &FleetConfig,
    filter: Option<&select::HostFilter>,
) -> std::collections::BTreeMap<String, tailscale::Presence> {
    match filter {
        Some(filter) if filter.needs_online() => presence_map(config),
        _ => tailscale::presence_for(config, None),
    }
}

/// Public CLI hook for `fleet status` and `fleet agents`.
pub fn run_status_command(
    config: &FleetConfig,
    names: &[String],
    filter: Option<&select::HostFilter>,
    env: &ProcessEnv,
    json: bool,
    agents_only: bool,
) -> Result<(), FleetError> {
    let presence = presence_map(config);
    let hosts = select_hosts(config, names, filter, &presence)?;
    let runner = remote::Runner::new(env);
    let agent_env = agents::AgentEnv::from_os();
    let snapshots = status::gather(config, &hosts, &presence, &runner, &agent_env);
    let now = agents::now_secs();
    if json {
        if agents_only {
            let rows: Vec<_> = snapshots
                .iter()
                .map(|snapshot| {
                    serde_json::json!({
                        "host": snapshot.host,
                        "reach": snapshot.reach,
                        "agents": snapshot.agents,
                    })
                })
                .collect();
            return write_json(&rows);
        }
        return write_json(&serde_json::json!({
            "current_host": config.current_host,
            "hosts": snapshots,
        }));
    }
    if agents_only {
        write_stdout(status::render_agents(&snapshots, now))
    } else {
        write_stdout(status::render_status(&config.current_host, &snapshots, now))
    }
}

/// Public CLI hook for `fleet agents --local`: this machine's records only,
/// without reading Fleet configuration. Remote status queries call this.
pub fn run_local_agents(json: bool) -> Result<(), FleetError> {
    let agent_env = agents::AgentEnv::from_os();
    let dir = agent_env.state_dir()?;
    let records = agents::local_agents(&dir, &agents::tmux_panes(), agents::now_secs())?;
    if json {
        return write_json(&records);
    }
    let snapshot = status::HostSnapshot {
        host: "local".into(),
        presence: tailscale::Presence::This,
        reach: status::Reach::Local,
        fleet: status::FleetPresence::Version(status::FLEET_VERSION.into()),
        tmux: status::TmuxPresence::Unknown,
        sessions: Vec::new(),
        agents: Some(records),
    };
    write_stdout(status::render_agents(&[snapshot], agents::now_secs()))
}

/// Public CLI hook for `fleet pick`: print the first matching host, or every
/// match with `all`, in configuration order.
pub fn run_pick_command(
    config: &FleetConfig,
    filter: Option<&select::HostFilter>,
    expression: Option<&str>,
    all: bool,
) -> Result<(), FleetError> {
    let presence = presence_for_filter(config, filter);
    let hosts = select_hosts(config, &[], filter, &presence)?;
    if hosts.is_empty() {
        return Err(FleetError::NoMatch(expression.unwrap_or("").to_string()));
    }
    let chosen = if all { &hosts[..] } else { &hosts[..1] };
    write_stdout(
        chosen
            .iter()
            .map(|host| format!("{host}\n"))
            .collect::<String>(),
    )
}

/// Public CLI hook for `fleet ports HOST`.
pub fn run_ports_command(
    config: &FleetConfig,
    host: &str,
    env: &ProcessEnv,
    json: bool,
) -> Result<(), FleetError> {
    let resolved = config
        .resolve(host)
        .ok_or_else(|| SshError::UnknownHost(host.to_string()))?;
    let endpoint = if resolved.is_local {
        remote::Endpoint::Local
    } else {
        remote::Endpoint::Ssh {
            target: resolved.ssh_target.clone(),
        }
    };
    let output = remote::Runner::new(env).run(&endpoint, &ports::ports_script());
    let Some(list) = ports::parse_ports(&output.stdout) else {
        let reason = if output.success() {
            "neither ss nor lsof is available".to_string()
        } else {
            let detail = output.stderr.trim();
            if detail.is_empty() {
                format!("query exited {}", output.code)
            } else {
                detail.to_string()
            }
        };
        return Err(remote::RemoteError::Transfer(format!(
            "could not list ports on {}: {reason}",
            resolved.canonical
        ))
        .into());
    };
    if json {
        write_json(&list)
    } else {
        write_stdout(ports::render_ports(&resolved.canonical, &list))
    }
}

/// `fleet list --json`.
pub fn render_list_json(config: &FleetConfig) -> serde_json::Value {
    let hosts: Vec<_> = config
        .hosts
        .iter()
        .map(|(name, host)| {
            serde_json::json!({
                "name": name,
                "current": *name == config.current_host,
                "ssh_target": host.ssh_target,
                "display_target": host.display_target(),
                "aliases": host.aliases,
                "os": host.os,
                "role": host.role,
                "user": host.user,
                "client_enrolled": host.client_enrolled,
                "gui": host.gui,
                "long_running_agents": host.long_running_agents,
                "tmux_session": host.tmux_session(),
                "t3code_port": host.t3code_port.map(Port::get),
                "tailscale_name": host.tailscale_name,
            })
        })
        .collect();
    serde_json::json!({ "current_host": config.current_host, "hosts": hosts })
}

/// Classify a forward PID using launchd job/child identity when configured.
/// Without a tunnel context (HOME unset), fall back to the conservative
/// mapped-port policy.
pub fn classify_managed_delete(
    config: &FleetConfig,
    env: &ProcessEnv,
    pid: u32,
    local_port: u16,
) -> ManagedClass {
    let mappings = mappings_from_config(config);
    let Ok(ctx) = tunnel_context(config, env, &mappings) else {
        return ConservativeManagedInspector::from_config(config).classify(pid, local_port);
    };
    match tunnels::managed_delete_verdict(
        pid,
        local_port,
        &ctx,
        &launchd::SystemLaunchctl::new(),
        &tunnels::SystemListenProbe::new(),
    ) {
        tunnels::ManagedDeleteVerdict::NotManaged => ManagedClass::Unmanaged,
        tunnels::ManagedDeleteVerdict::Managed { port } => {
            let label = mappings
                .iter()
                .find(|mapping| mapping.local_port == port)
                .map(|mapping| mapping.label.clone())
                .unwrap_or_default();
            ManagedClass::Managed { port, label }
        }
        tunnels::ManagedDeleteVerdict::Ambiguous { .. } => ManagedClass::Ambiguous,
    }
}

/// Production `forward delete` inspector. Uses launchd identity when possible.
pub struct ConfiguredInspector<'a> {
    pub config: &'a FleetConfig,
    pub env: &'a ProcessEnv,
}

impl ManagedForwardInspector for ConfiguredInspector<'_> {
    fn classify(&self, pid: u32, local_port: u16) -> ManagedClass {
        classify_managed_delete(self.config, self.env, pid, local_port)
    }
}
