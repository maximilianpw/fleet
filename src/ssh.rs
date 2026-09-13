//! SSH, tmux, shell, run, and T3 argument construction.
//!
//! Configured hosts use `ssh_target` / `tmux_target` / `forward_target` after
//! alias_targets resolution. Unknown tokens keep the legacy pass-through:
//! `ssh unknown` uses `tm-unknown`, `shell`/`run` use `unknown`, and
//! `forward` uses `fleet-forward-unknown`. `t3` still requires declared
//! metadata. Local attachment uses PATH `tmux` and session `main` by default,
//! not the remote tmux_command/tmux_session.

use thiserror::Error;

use crate::config::{
    is_safe_session_name, parse_port, validate_ssh_target, FleetConfig, PortError, TargetError,
};
use crate::process::{PlannedCommand, ProcessEnv};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentForward {
    pub local_port: u16,
    pub remote_port: u16,
}

impl AttachmentForward {
    pub fn parse(spec: &str) -> Result<Self, SshError> {
        if let Some((local, remote)) = spec.split_once(':') {
            if remote.contains(':') {
                return Err(SshError::ForwardSpecInvalid);
            }
            Ok(Self {
                local_port: parse_port(local)?,
                remote_port: parse_port(remote)?,
            })
        } else {
            let port = parse_port(spec)?;
            Ok(Self {
                local_port: port,
                remote_port: port,
            })
        }
    }

    pub fn spec(&self) -> String {
        format!(
            "127.0.0.1:{}:localhost:{}",
            self.local_port, self.remote_port
        )
    }
}

#[derive(Debug, Error)]
pub enum SshError {
    #[error("fleet: session names may only contain A-Z, a-z, 0-9, _, ., and -")]
    InvalidSession,
    #[error("fleet: --forward requires PORT or LOCAL_PORT:REMOTE_PORT")]
    ForwardRequiresValue,
    #[error("fleet: SSH forwards use PORT or LOCAL_PORT:REMOTE_PORT")]
    ForwardSpecInvalid,
    #[error("fleet: unknown SSH option: {0}")]
    UnknownSshOption(String),
    #[error("fleet: expected at most one tmux session name")]
    MultipleSessions,
    #[error("fleet: --forward requires a remote Fleet host")]
    ForwardOnLocalHost,
    #[error("fleet: host does not declare a T3 Code port: {0}")]
    MissingT3Port(String),
    #[error("fleet: unknown Fleet host: {0}")]
    UnknownHost(String),
    #[error("fleet: run requires a command")]
    MissingRunCommand,
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Target(#[from] TargetError),
}

impl SshError {
    pub fn exit_code(&self) -> i32 {
        2
    }
}

pub fn validate_session(name: &str) -> Result<(), SshError> {
    if is_safe_session_name(name) {
        Ok(())
    } else {
        Err(SshError::InvalidSession)
    }
}

pub fn parse_ssh_tail(
    args: &[String],
) -> Result<(Option<String>, Vec<AttachmentForward>), SshError> {
    let mut session = None;
    let mut forwards = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--forward" {
            let spec = args.get(index + 1).ok_or(SshError::ForwardRequiresValue)?;
            forwards.push(AttachmentForward::parse(spec)?);
            index += 2;
            continue;
        }
        if arg.starts_with("--") {
            return Err(SshError::UnknownSshOption(arg.clone()));
        }
        if session.is_some() {
            return Err(SshError::MultipleSessions);
        }
        validate_session(arg)?;
        session = Some(arg.clone());
        index += 1;
    }
    Ok((session, forwards))
}

pub fn plan_ssh(
    config: &FleetConfig,
    host: &str,
    session: Option<&str>,
    forwards: &[AttachmentForward],
) -> Result<PlannedCommand, SshError> {
    validate_ssh_target(host)?;
    if let Some(resolved) = config.resolve(host) {
        if resolved.is_local {
            if !forwards.is_empty() {
                return Err(SshError::ForwardOnLocalHost);
            }
            let session = session.unwrap_or("main");
            validate_session(session)?;
            return Ok(PlannedCommand::tmux(vec![
                "new-session".into(),
                "-A".into(),
                "-s".into(),
                session.into(),
            ]));
        }

        let forward_args = attachment_forward_args(forwards);
        if let Some(session) = session {
            validate_session(session)?;
            return Ok(PlannedCommand::ssh(
                ssh_named_session_args(
                    &forward_args,
                    &resolved.ssh_target,
                    &resolved.tmux_command,
                    session,
                ),
                "open a remote SSH session",
            ));
        }
        if let Some(tmux_target) = resolved.tmux_target {
            let mut args = forward_args;
            args.push(tmux_target);
            return Ok(PlannedCommand::ssh(args, "open a remote SSH session"));
        }
        return Ok(PlannedCommand::ssh(
            ssh_named_session_args(
                &forward_args,
                &resolved.ssh_target,
                &resolved.tmux_command,
                &resolved.tmux_session,
            ),
            "open a remote SSH session",
        ));
    }

    let forward_args = attachment_forward_args(forwards);
    if let Some(session) = session {
        validate_session(session)?;
        return Ok(PlannedCommand::ssh(
            ssh_named_session_args(&forward_args, host, "tmux", session),
            "open a remote SSH session",
        ));
    }
    let mut args = forward_args;
    args.push(format!("tm-{host}"));
    Ok(PlannedCommand::ssh(args, "open a remote SSH session"))
}

pub fn plan_shell(
    config: &FleetConfig,
    host: &str,
    extra: &[String],
    env: &ProcessEnv,
) -> Result<PlannedCommand, SshError> {
    validate_ssh_target(host)?;
    if config.is_local_token(host) {
        return Ok(PlannedCommand::program(
            env.shell_program(),
            Vec::new(),
            "open a local shell",
        ));
    }
    let target = config
        .resolve(host)
        .map(|resolved| resolved.ssh_target)
        .unwrap_or_else(|| host.to_string());
    let mut args = vec![target];
    args.extend(extra.iter().cloned());
    Ok(PlannedCommand::ssh(args, "open a remote shell"))
}

pub fn plan_run(
    config: &FleetConfig,
    host: &str,
    command: &[String],
) -> Result<PlannedCommand, SshError> {
    validate_ssh_target(host)?;
    let Some((program, args)) = command.split_first() else {
        return Err(SshError::MissingRunCommand);
    };
    if config.is_local_token(host) {
        return Ok(PlannedCommand::program(
            program.clone(),
            args.to_vec(),
            "run a local command",
        ));
    }
    let target = config
        .resolve(host)
        .map(|resolved| resolved.ssh_target)
        .unwrap_or_else(|| host.to_string());
    let mut args = vec![target];
    args.extend(command.iter().cloned());
    Ok(PlannedCommand::ssh(args, "run a remote command"))
}

pub fn plan_t3(
    config: &FleetConfig,
    host: &str,
    local_port: Option<u16>,
) -> Result<PlannedCommand, SshError> {
    validate_ssh_target(host)?;
    let Some(resolved) = config.resolve(host) else {
        return Err(SshError::UnknownHost(host.to_string()));
    };
    let Some(remote_port) = resolved.t3code_port else {
        return Err(SshError::MissingT3Port(host.to_string()));
    };
    let local_port = local_port.unwrap_or(remote_port);
    let spec = format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}");
    Ok(dedicated_forward_command(&spec, &resolved.forward_target))
}

pub fn plan_ad_hoc_forward(
    config: &FleetConfig,
    host: &str,
    local_port: u16,
    remote_port: u16,
    remote_host: &str,
) -> Result<PlannedCommand, SshError> {
    validate_ssh_target(host)?;
    let target = config
        .resolve(host)
        .map(|resolved| resolved.forward_target)
        .unwrap_or_else(|| format!("fleet-forward-{host}"));
    let remote_host = if remote_host.is_empty() {
        "localhost"
    } else {
        remote_host
    };
    let spec = format!("127.0.0.1:{local_port}:{remote_host}:{remote_port}");
    Ok(dedicated_forward_command(&spec, &target))
}

pub fn dedicated_forward_command(spec: &str, ssh_target: &str) -> PlannedCommand {
    PlannedCommand::ssh(
        vec![
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "ForwardAgent=no".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPath=none".into(),
            "-N".into(),
            "-L".into(),
            spec.into(),
            ssh_target.into(),
        ],
        "open an SSH forward",
    )
}

fn attachment_forward_args(forwards: &[AttachmentForward]) -> Vec<String> {
    if forwards.is_empty() {
        return Vec::new();
    }
    let mut args = vec![
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        "ControlPath=none".into(),
    ];
    for forward in forwards {
        args.push("-L".into());
        args.push(forward.spec());
    }
    args
}

fn ssh_named_session_args(
    forward_args: &[String],
    ssh_target: &str,
    tmux_command: &str,
    session: &str,
) -> Vec<String> {
    let mut args = vec!["-t".into()];
    args.extend(forward_args.iter().cloned());
    args.push(ssh_target.to_string());
    args.push(format!("{tmux_command} new-session -A -s '{session}'"));
    args
}

pub fn local_ports_of(forwards: &[AttachmentForward]) -> Vec<u16> {
    forwards.iter().map(|forward| forward.local_port).collect()
}
