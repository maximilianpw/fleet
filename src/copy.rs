//! Direct file copy between the current machine and a declared Fleet host.
//!
//! The public syntax follows `scp`: `HOST:PATH` denotes a remote endpoint,
//! while a declared remote host without a path is accepted as destination
//! shorthand for that host's home directory. Fleet resolves only declared
//! canonical names and aliases, then execs OpenSSH `scp` without a local
//! shell. Recursive and remote-to-remote copies are intentionally excluded.

use thiserror::Error;

use crate::config::{validate_ssh_target, FleetConfig, TargetError};
use crate::process::PlannedCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointRole {
    Source,
    Destination,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CopyEndpoint {
    Local(String),
    Remote { ssh_target: String, path: String },
}

#[derive(Debug, Error)]
pub enum CopyError {
    #[error("fleet: copy remote host is not declared: {0}")]
    UnknownRemoteHost(String),
    #[error("fleet: copy remote source requires HOST:PATH")]
    RemoteSourcePathRequired,
    #[error("fleet: copy paths must not be empty")]
    EmptyPath,
    #[error("fleet: copy paths must not contain control characters")]
    ControlCharacter,
    #[error("fleet: copy requires exactly one local endpoint and one remote Fleet endpoint")]
    RequiresLocalAndRemote,
    #[error(transparent)]
    Target(#[from] TargetError),
}

impl CopyError {
    pub fn exit_code(&self) -> i32 {
        2
    }
}

/// Plan an OpenSSH `scp` transfer between one local path and one declared
/// remote Fleet host. Fleet passes paths as argv and never constructs a local
/// shell command. OpenSSH's default SFTP-backed scp mode owns remote path
/// interpretation and transfer diagnostics.
pub fn plan_copy(
    config: &FleetConfig,
    source: &str,
    destination: &str,
) -> Result<PlannedCommand, CopyError> {
    let source = parse_copy_endpoint(config, source, EndpointRole::Source)?;
    let destination = parse_copy_endpoint(config, destination, EndpointRole::Destination)?;

    if matches!(source, CopyEndpoint::Local(_)) == matches!(destination, CopyEndpoint::Local(_)) {
        return Err(CopyError::RequiresLocalAndRemote);
    }

    Ok(PlannedCommand::program(
        "scp".into(),
        vec![
            "--".into(),
            render_copy_endpoint(source),
            render_copy_endpoint(destination),
        ],
        "copy a file between Fleet machines",
    ))
}

fn parse_copy_endpoint(
    config: &FleetConfig,
    raw: &str,
    role: EndpointRole,
) -> Result<CopyEndpoint, CopyError> {
    validate_copy_path(raw)?;

    if let Some((host, path)) = remote_spec(raw) {
        validate_ssh_target(host)?;
        let resolved = config
            .resolve(host)
            .ok_or_else(|| CopyError::UnknownRemoteHost(host.to_string()))?;
        if path.is_empty() && role == EndpointRole::Source {
            return Err(CopyError::RemoteSourcePathRequired);
        }
        if resolved.is_local {
            let local_path = if path.is_empty() { "." } else { path };
            return Ok(CopyEndpoint::Local(local_path.to_string()));
        }
        return Ok(CopyEndpoint::Remote {
            ssh_target: resolved.ssh_target,
            path: path.to_string(),
        });
    }

    if role == EndpointRole::Destination {
        if let Some(resolved) = config.resolve(raw) {
            if !resolved.is_local {
                return Ok(CopyEndpoint::Remote {
                    ssh_target: resolved.ssh_target,
                    path: String::new(),
                });
            }
        }
    }

    Ok(CopyEndpoint::Local(raw.to_string()))
}

fn remote_spec(raw: &str) -> Option<(&str, &str)> {
    let (host, path) = raw.split_once(':')?;
    if host.is_empty() || host.contains('/') || matches!(host, "." | "..") {
        return None;
    }
    Some((host, path))
}

fn validate_copy_path(path: &str) -> Result<(), CopyError> {
    if path.is_empty() {
        return Err(CopyError::EmptyPath);
    }
    if path.chars().any(char::is_control) {
        return Err(CopyError::ControlCharacter);
    }
    Ok(())
}

fn render_copy_endpoint(endpoint: CopyEndpoint) -> String {
    match endpoint {
        CopyEndpoint::Local(path) => path,
        CopyEndpoint::Remote { ssh_target, path } => format!("{ssh_target}:{path}"),
    }
}
