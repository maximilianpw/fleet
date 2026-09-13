//! Fleet configuration, SSH/tmux argument construction, and ad-hoc forwards.
//!
//! The `fleet` binary loads TOML, validates it, and execs OpenSSH or tmux.
//! Tunnel supervision, doctor, and the runner live in sibling modules.

use std::io;

pub mod config;
pub mod doctor;
pub mod forwards;
pub mod launchd;
pub mod process;
pub mod runner;
pub mod ssh;
pub mod tunnels;

pub use config::{
    load_config, parse_config, parse_port, resolve_config_origin, ConfigError, ConfigOrigin,
    FleetConfig, HostConfig, Port, PortError, ResolvedHost, Supervisor, TargetError, TunnelMapping,
    TunnelsConfig, SCHEMA_VERSION,
};
pub use forwards::{
    collect_forward_rows, ensure_ports_free, forward_local_port, parse_forward_pids,
    render_forward_list, stop_forwards, ConservativeManagedInspector, ForwardError, ForwardRow,
    ManagedClass, ManagedForwardInspector, SnapshotInspector,
};
pub use process::{
    exec_replace, parse_ps_output, PathKill, PathPsTable, PlannedCommand, ProcessEnv, ProcessError,
    ProcessTable, RecordingSignals, SignalSender, SnapshotProcesses,
};
pub use ssh::{
    local_ports_of, parse_ssh_tail, plan_ad_hoc_forward, plan_run, plan_shell, plan_ssh, plan_t3,
    AttachmentForward, SshError,
};

/// Help text preserved from the legacy CLI, plus `config validate` and
/// `completions`.
pub const USAGE: &str = "\
usage:
  fleet list
  fleet ssh HOST [SESSION] [--forward PORT|LOCAL_PORT:REMOTE_PORT]...
  fleet shell HOST
  fleet run HOST COMMAND...
  fleet forward HOST LOCAL_PORT REMOTE_PORT [REMOTE_HOST]
  fleet forward list [LOCAL_PORT]
  fleet forward stop PID...
  fleet forward delete PID...
  fleet t3 HOST [LOCAL_PORT]
  fleet tunnel status
  fleet tunnel pause PORT
  fleet tunnel resume PORT
  fleet doctor HOST
  fleet config validate
  fleet completions SHELL

examples:
  fleet ssh workbox
  fleet ssh workbox agents --forward 3000 --forward 5173
  fleet shell workbox
  fleet run workbox btop
  fleet forward workbox 3000 3000
  fleet forward list 3000
  fleet forward delete 12345
  fleet t3 workbox 51001
  fleet tunnel status
  fleet tunnel pause 3000
  fleet tunnel resume 3000
  fleet doctor workbox";

#[derive(Debug, thiserror::Error)]
pub enum FleetError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Ssh(#[from] SshError),
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
    #[error("{0}")]
    Usage(String),
}

impl FleetError {
    pub fn usage() -> Self {
        Self::Usage(USAGE.to_string())
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Config(error) => error.exit_code(),
            Self::Ssh(error) => error.exit_code(),
            Self::Forward(error) => error.exit_code(),
            Self::Process(error) => error.exit_code(),
            Self::Port(error) => error.exit_code(),
            Self::Target(error) => error.exit_code(),
            Self::Tunnel(error) => error.exit_code(),
            Self::Doctor(error) => error.exit_code(),
            Self::Usage(_) => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelCommand {
    Status,
    Pause { port: u16 },
    Resume { port: u16 },
}

pub fn mappings_from_config(config: &FleetConfig) -> Vec<tunnels::Mapping> {
    config
        .tunnels
        .mappings
        .iter()
        .map(|mapping| tunnels::Mapping {
            local_port: mapping.local_port.get(),
            host: mapping.host.clone(),
            remote_port: mapping.remote_port.get(),
            remote_host: mapping.remote_host.clone(),
            label: mapping.label.clone(),
        })
        .collect()
}

pub fn doctor_mappings_from_config(config: &FleetConfig) -> Vec<tunnels::Mapping> {
    config
        .tunnels
        .mappings
        .iter()
        .map(|mapping| tunnels::Mapping {
            local_port: mapping.local_port.get(),
            host: config
                .resolve(&mapping.host)
                .map(|resolved| resolved.canonical)
                .unwrap_or_else(|| mapping.host.clone()),
            remote_port: mapping.remote_port.get(),
            remote_host: mapping.remote_host.clone(),
            label: mapping.label.clone(),
        })
        .collect()
}

pub fn supervisor_from_config(config: &FleetConfig) -> tunnels::Supervisor {
    match config.tunnels.supervisor {
        Supervisor::None => tunnels::Supervisor::None,
        Supervisor::Launchd => tunnels::Supervisor::Launchd,
    }
}

fn tunnel_context<'a>(
    config: &'a FleetConfig,
    env: &'a ProcessEnv,
    mappings: &'a [tunnels::Mapping],
) -> Result<tunnels::TunnelContext<'a>, FleetError> {
    let home = env.home.as_deref().ok_or(ConfigError::HomeNotSet)?;
    Ok(tunnels::TunnelContext {
        supervisor: supervisor_from_config(config),
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
        TunnelCommand::Status => {
            tunnels::print_status(&mut io::stdout(), &ctx, &launchd, &listen)?;
        }
        TunnelCommand::Pause { port } => {
            let message = tunnels::pause(port, &ctx, &launchd)?;
            print!("{message}");
        }
        TunnelCommand::Resume { port } => {
            let message = tunnels::resume(port, &ctx, &launchd, &listen)?;
            print!("{message}");
        }
    }
    Ok(())
}

/// Public CLI hook for `fleet doctor HOST`.
///
/// Unknown hosts are rejected by the CLI before this hook runs.
pub fn run_doctor_command(
    config: &FleetConfig,
    host: &str,
    env: &ProcessEnv,
) -> Result<(), FleetError> {
    let resolved = config
        .resolve(host)
        .ok_or_else(|| SshError::UnknownHost(host.to_string()))?;
    let mappings = doctor_mappings_from_config(config);
    let ctx = tunnel_context(config, env, &mappings)?;
    let doctor_host = doctor::DoctorHost {
        canonical: resolved.canonical,
        ssh_target: resolved.ssh_target,
        is_local: resolved.is_local,
    };
    doctor::doctor(
        &mut io::stdout(),
        &doctor_host,
        &ctx,
        &launchd::SystemLaunchctl::new(),
        &tunnels::SystemListenProbe::new(),
        &doctor::SystemSsh::new(),
        doctor::DoctorDeadlines::production(),
    )?;
    Ok(())
}

/// Conservative inspector that does not talk to launchd. Tests use this when
/// they want mapping-port policy without a job snapshot.
pub fn managed_inspector_for(config: &FleetConfig) -> ConservativeManagedInspector {
    ConservativeManagedInspector::from_config(config)
}

/// Classify a forward PID using launchd job/child identity when configured.
pub fn classify_managed_delete(
    config: &FleetConfig,
    env: &ProcessEnv,
    pid: u32,
    local_port: u16,
) -> ManagedClass {
    let mappings = mappings_from_config(config);
    let Ok(ctx) = tunnel_context(config, env, &mappings) else {
        return if config.tunnels.supervisor == Supervisor::Launchd
            && config.mapped_local_ports().contains(&local_port)
        {
            ManagedClass::Ambiguous
        } else {
            ManagedClass::Unmanaged
        };
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
