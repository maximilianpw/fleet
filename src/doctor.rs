//! `fleet doctor` SSH reachability, remote TCP probes, and health reporting.
//!
//! Auth failure, unavailability, remote-app-down, and probe-unknown are
//! distinct. A local listener is not app readiness. Paused mappings skip the
//! remote probe and do not fail doctor by themselves.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::Command;
use std::time::Duration;

use thiserror::Error;

use crate::launchd::{self, Launchctl};
use crate::tunnels::{
    self, has_managed_tunnels, inspect, mappings_for_host, require_launchctl, ListenProbe,
    ListenerOwnership, Mapping, Supervisor, SupervisorState, TunnelContext, NO_TUNNELS_CONFIGURED,
};

pub const DOCTOR_SSH_TIMEOUT: Duration = Duration::from_secs(15);
pub const DOCTOR_SSH_KILL_AFTER: Duration = Duration::from_secs(2);
pub const DOCTOR_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorDeadlines {
    pub ssh: Duration,
    pub ssh_kill_after: Duration,
    pub probe: Duration,
    pub probe_kill_after: Duration,
    pub poll: Duration,
}

impl DoctorDeadlines {
    pub fn production() -> Self {
        Self {
            ssh: DOCTOR_SSH_TIMEOUT,
            ssh_kill_after: DOCTOR_SSH_KILL_AFTER,
            probe: DOCTOR_SSH_TIMEOUT,
            probe_kill_after: DOCTOR_SSH_KILL_AFTER,
            poll: DOCTOR_POLL,
        }
    }
}

impl Default for DoctorDeadlines {
    fn default() -> Self {
        Self::production()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshState {
    Skipped,
    Reachable,
    Auth,
    Unavailable,
}

impl SshState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Reachable => "reachable",
            Self::Auth => "auth",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteListen {
    Skipped,
    Listening,
    Down,
    Unknown,
}

impl RemoteListen {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Listening => "listening",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorHost {
    pub canonical: String,
    pub ssh_target: String,
    pub is_local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshRequest {
    pub target: String,
    pub remote_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshOutcome {
    pub code: i32,
    pub stderr: String,
}

/// Seam for doctor SSH. Tests inject a fake; production uses [`SystemSsh`].
pub trait SshProbe {
    fn run(&self, request: &SshRequest, deadlines: DoctorDeadlines) -> SshOutcome;
}

impl<T: SshProbe + ?Sized> SshProbe for &T {
    fn run(&self, request: &SshRequest, deadlines: DoctorDeadlines) -> SshOutcome {
        (**self).run(request, deadlines)
    }
}

pub struct SystemSsh {
    program: OsString,
}

impl SystemSsh {
    pub fn new() -> Self {
        Self {
            program: OsString::from("ssh"),
        }
    }

    pub fn with_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for SystemSsh {
    fn default() -> Self {
        Self::new()
    }
}

impl SshProbe for SystemSsh {
    fn run(&self, request: &SshRequest, deadlines: DoctorDeadlines) -> SshOutcome {
        let mut cmd = Command::new(&self.program);
        cmd.args(ssh_argv(&request.target, request.remote_command.as_deref()));
        let (timeout, kill_after) = if request.remote_command.is_some() {
            (deadlines.probe, deadlines.probe_kill_after)
        } else {
            (deadlines.ssh, deadlines.ssh_kill_after)
        };
        match launchd::run_with_deadline(&mut cmd, timeout, kill_after, deadlines.poll) {
            Ok(output) => SshOutcome {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(err) => SshOutcome {
                code: 255,
                stderr: format!("failed to spawn ssh: {err}"),
            },
        }
    }
}

/// OpenSSH argv for doctor probes. `-n` keeps SSH from consuming stdin so a
/// mapping loop cannot skip later ports.
pub fn ssh_argv(target: &str, remote_command: Option<&str>) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-n"),
        OsString::from("-o"),
        OsString::from("BatchMode=yes"),
        OsString::from("-o"),
        OsString::from("ConnectTimeout=10"),
        OsString::from("-o"),
        OsString::from("ControlMaster=no"),
        OsString::from("-o"),
        OsString::from("ControlPath=none"),
        OsString::from("-o"),
        OsString::from("ForwardAgent=no"),
        OsString::from(target),
    ];
    match remote_command {
        Some(command) => args.push(OsString::from(command)),
        None => args.push(OsString::from(":")),
    }
    args
}

/// Remote command passed as one SSH argument so a fish login shell can parse
/// it. Host and port are interpolated only after [`remote_host_is_valid`].
pub fn remote_listen_command(remote_host: &str, remote_port: u16) -> String {
    format!(
        "bash -c 'command -v timeout >/dev/null || exit 69; if timeout 3 bash -c \"exec 3<>/dev/tcp/{remote_host}/{remote_port}\"; then exit 0; else exit 1; fi'"
    )
}

pub fn remote_host_is_valid(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

pub fn classify_ssh_stderr(stderr: &str) -> SshState {
    const AUTH: &[&str] = &[
        "Permission denied",
        "publickey",
        "Too many authentication",
        "Host key verification failed",
        "No more authentication methods",
    ];
    if AUTH.iter().any(|needle| stderr.contains(needle)) {
        return SshState::Auth;
    }
    SshState::Unavailable
}

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error("fleet: invalid remote tunnel host: {host}")]
    InvalidRemoteHost { host: String },
    #[error("{}", tunnels::LAUNCHCTL_REQUIRED_MESSAGE)]
    LaunchctlRequired,
    /// Probe finished and printed diagnostics; the process should exit 1.
    /// Display is empty so the CLI does not add a second summary line.
    #[error("")]
    Unhealthy,
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl DoctorError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidRemoteHost { .. } => 2,
            Self::LaunchctlRequired | Self::Unhealthy | Self::Io(_) => 1,
        }
    }
}

pub fn doctor(
    out: &mut dyn Write,
    host: &DoctorHost,
    ctx: &TunnelContext<'_>,
    launchd: &impl Launchctl,
    listen: &impl ListenProbe,
    ssh: &impl SshProbe,
    deadlines: DoctorDeadlines,
) -> Result<(), DoctorError> {
    let mut unhealthy = false;
    let ssh_state = if host.is_local {
        writeln!(out, "SSH: skipped (current machine)").map_err(DoctorError::Io)?;
        SshState::Skipped
    } else {
        let outcome = ssh.run(
            &SshRequest {
                target: host.ssh_target.clone(),
                remote_command: None,
            },
            deadlines,
        );
        if outcome.code == 0 {
            writeln!(out, "SSH: reachable").map_err(DoctorError::Io)?;
            SshState::Reachable
        } else {
            match classify_ssh_stderr(&outcome.stderr) {
                SshState::Auth => {
                    writeln!(out, "SSH: auth failure").map_err(DoctorError::Io)?;
                    writeln!(
                        out,
                        "unhealthy: SSH to {} failed authentication",
                        host.canonical
                    )
                    .map_err(DoctorError::Io)?;
                    unhealthy = true;
                    SshState::Auth
                }
                _ => {
                    writeln!(out, "SSH: unavailable").map_err(DoctorError::Io)?;
                    writeln!(out, "unhealthy: SSH to {} is unavailable", host.canonical)
                        .map_err(DoctorError::Io)?;
                    unhealthy = true;
                    SshState::Unavailable
                }
            }
        }
    };

    if ctx.supervisor == Supervisor::Launchd {
        require_launchctl(launchd).map_err(|_| DoctorError::LaunchctlRequired)?;
    }

    let host_mappings = mappings_for_host(ctx.mappings, &host.canonical);
    if host_mappings.is_empty() {
        if has_managed_tunnels(ctx.mappings) {
            writeln!(out, "No managed tunnels target {}.", host.canonical)
                .map_err(DoctorError::Io)?;
        } else {
            writeln!(out, "{NO_TUNNELS_CONFIGURED}").map_err(DoctorError::Io)?;
        }
        return if unhealthy {
            Err(DoctorError::Unhealthy)
        } else {
            Ok(())
        };
    }

    for mapping in host_mappings {
        diagnose_mapping(
            out,
            host,
            mapping,
            ctx,
            launchd,
            listen,
            ssh,
            deadlines,
            ssh_state,
            &mut unhealthy,
        )?;
    }

    if unhealthy {
        Err(DoctorError::Unhealthy)
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn diagnose_mapping(
    out: &mut dyn Write,
    host: &DoctorHost,
    mapping: &Mapping,
    ctx: &TunnelContext<'_>,
    launchd: &impl Launchctl,
    listen: &impl ListenProbe,
    ssh: &impl SshProbe,
    deadlines: DoctorDeadlines,
    ssh_state: SshState,
    unhealthy: &mut bool,
) -> Result<(), DoctorError> {
    let obs = inspect(mapping, ctx.supervisor, ctx.uid, launchd, listen);
    let mut remote = RemoteListen::Skipped;
    if ssh_state == SshState::Reachable && obs.state() != SupervisorState::Paused {
        if !remote_host_is_valid(&mapping.remote_host) {
            return Err(DoctorError::InvalidRemoteHost {
                host: mapping.remote_host.clone(),
            });
        }
        let outcome = ssh.run(
            &SshRequest {
                target: host.ssh_target.clone(),
                remote_command: Some(remote_listen_command(
                    &mapping.remote_host,
                    mapping.remote_port,
                )),
            },
            deadlines,
        );
        remote = match outcome.code {
            0 => RemoteListen::Listening,
            1 => RemoteListen::Down,
            code => {
                writeln!(
                    out,
                    "unhealthy: remote TCP probe failed for {} (SSH/probe exit {code}); app state unknown",
                    host.canonical
                )
                .map_err(DoctorError::Io)?;
                *unhealthy = true;
                RemoteListen::Unknown
            }
        };
    }

    writeln!(
        out,
        "tunnel {}: supervisor={} local={} remote={}",
        mapping.local_port,
        obs.state().as_str(),
        obs.listen.as_str(),
        remote.as_str()
    )
    .map_err(DoctorError::Io)?;

    if obs.state() == SupervisorState::Paused {
        return Ok(());
    }
    if obs.state() == SupervisorState::Unsupervised {
        writeln!(
            out,
            "unhealthy: managed tunnel for local port {} is not supervised on this host",
            mapping.local_port
        )
        .map_err(DoctorError::Io)?;
        *unhealthy = true;
        return Ok(());
    }
    if obs.state() != SupervisorState::Running {
        writeln!(
            out,
            "unhealthy: managed tunnel for local port {} is not running",
            mapping.local_port
        )
        .map_err(DoctorError::Io)?;
        *unhealthy = true;
    }
    match obs.listen {
        ListenerOwnership::Unrelated => {
            writeln!(
                out,
                "unhealthy: local port {} is occupied by an unrelated process",
                mapping.local_port
            )
            .map_err(DoctorError::Io)?;
            *unhealthy = true;
        }
        ListenerOwnership::Unknown => {
            writeln!(
                out,
                "unhealthy: local port {} listener ownership could not be verified",
                mapping.local_port
            )
            .map_err(DoctorError::Io)?;
            *unhealthy = true;
        }
        ListenerOwnership::None if obs.state() == SupervisorState::Running => {
            writeln!(
                out,
                "unhealthy: managed tunnel for local port {} is not listening on 127.0.0.1",
                mapping.local_port
            )
            .map_err(DoctorError::Io)?;
            *unhealthy = true;
        }
        ListenerOwnership::None | ListenerOwnership::Owned => {}
    }
    if remote == RemoteListen::Down {
        writeln!(
            out,
            "unhealthy: remote app is not listening on {} {}:{}",
            host.canonical, mapping.remote_host, mapping.remote_port
        )
        .map_err(DoctorError::Io)?;
        *unhealthy = true;
    }
    Ok(())
}
