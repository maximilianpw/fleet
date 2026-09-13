//! Managed tunnel observation, pause/resume, and listener ownership.
//!
//! Supervisor state, listener ownership, and remote probes are separate.
//! Mutations refresh launchd after each change; a failed probe is not treated
//! as a successful absence check.

use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use thiserror::Error;

use crate::launchd::{self, gui_target, job_is_loaded, launch_agent_plist, Launchctl};

pub const NO_SUPERVISOR_MESSAGE: &str =
    "fleet: managed tunnels are supervised by macOS launchd; this host has no tunnel supervisor.";
pub const LAUNCHCTL_REQUIRED_MESSAGE: &str =
    "fleet: launchctl is required to manage Darwin tunnel jobs";
pub const MACOS_ONLY_MESSAGE: &str = "Managed tunnel jobs are installed only on macOS (launchd).";
pub const NO_TUNNELS_CONFIGURED: &str = "No managed tunnels configured on this host.";

const LISTEN_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const LISTEN_PROBE_KILL_AFTER: Duration = Duration::from_secs(1);
const LISTEN_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervisor {
    None,
    Launchd,
}

impl Supervisor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Launchd => "launchd",
        }
    }

    pub fn from_config(name: &str) -> Option<Self> {
        match name {
            "none" => Some(Self::None),
            "launchd" => Some(Self::Launchd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub local_port: u16,
    pub host: String,
    pub remote_port: u16,
    pub remote_host: String,
    pub label: String,
}

impl Mapping {
    pub fn remote_display(&self) -> String {
        format!("{}:{}:{}", self.host, self.remote_host, self.remote_port)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorState {
    Running,
    Stopped,
    Paused,
    Unsupervised,
    Unknown,
}

impl SupervisorState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Paused => "paused",
            Self::Unsupervised => "unsupervised",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerOwnership {
    Owned,
    Unrelated,
    None,
    Unknown,
}

impl ListenerOwnership {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Unrelated => "unrelated",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSnapshot {
    pub state: SupervisorState,
    pub loaded: Option<bool>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub snapshot: JobSnapshot,
    pub listen: ListenerOwnership,
}

impl Observation {
    pub fn state(&self) -> SupervisorState {
        self.snapshot.state
    }
}

pub struct TunnelContext<'a> {
    pub supervisor: Supervisor,
    pub mappings: &'a [Mapping],
    pub home: &'a Path,
    pub uid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedDeleteVerdict {
    NotManaged,
    Managed { port: u16 },
    Ambiguous { port: u16 },
}

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("{}", NO_SUPERVISOR_MESSAGE)]
    NoSupervisor,
    #[error("{}", LAUNCHCTL_REQUIRED_MESSAGE)]
    LaunchctlRequired,
    #[error("fleet: no managed tunnel for local port {port}")]
    NoMapping { port: u16 },
    #[error(
        "fleet: launchd plist missing: {} (re-apply Home Manager on this Mac)",
        .path.display()
    )]
    PlistMissing { path: PathBuf },
    #[error(
        "fleet: local port {port} is occupied by an unrelated process; not killing it.\n\
         fleet: stop that listener, then retry resume. Pause remains in effect."
    )]
    PortOccupied { port: u16 },
    #[error(
        "fleet: could not verify local port {port} listener ownership; not changing pause state."
    )]
    ListenerUnknown { port: u16 },
    #[error("fleet: failed to persist pause for {target}")]
    DisableFailed { target: String },
    #[error("fleet: failed to stop managed tunnel job {target}")]
    BootoutFailed { target: String },
    #[error("fleet: failed to clear pause for {target}")]
    EnableFailed { target: String },
    #[error("fleet: failed to load managed tunnel job {target}")]
    BootstrapFailed { target: String },
    #[error("fleet: failed to start managed tunnel job {target}")]
    KickstartFailed { target: String },
    #[error("fleet: could not read launchd state for {target}")]
    JobStateUnknown { target: String },
}

impl TunnelError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NoSupervisor | Self::NoMapping { .. } => 2,
            Self::LaunchctlRequired
            | Self::PlistMissing { .. }
            | Self::PortOccupied { .. }
            | Self::ListenerUnknown { .. }
            | Self::DisableFailed { .. }
            | Self::BootoutFailed { .. }
            | Self::EnableFailed { .. }
            | Self::BootstrapFailed { .. }
            | Self::KickstartFailed { .. }
            | Self::JobStateUnknown { .. } => 1,
        }
    }
}

/// Listener PID inspection. Empty lsof output is not the same as a failed
/// inspection: lsof exits 1 when nothing matches.
pub trait ListenProbe {
    fn listening_pids(&self, port: u16) -> Result<Vec<u32>, ListenError>;
    fn loopback_connects(&self, port: u16) -> bool;
    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity>;
}

#[derive(Debug)]
pub struct ListenError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub ppid: Option<u32>,
    pub comm: String,
}

impl<T: ListenProbe + ?Sized> ListenProbe for &T {
    fn listening_pids(&self, port: u16) -> Result<Vec<u32>, ListenError> {
        (**self).listening_pids(port)
    }

    fn loopback_connects(&self, port: u16) -> bool {
        (**self).loopback_connects(port)
    }

    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        (**self).process_identity(pid)
    }
}

pub struct SystemListenProbe {
    lsof_program: std::ffi::OsString,
    ps_program: std::ffi::OsString,
    tcp_timeout: Duration,
    lsof_timeout: Duration,
    lsof_kill_after: Duration,
}

impl SystemListenProbe {
    pub fn new() -> Self {
        Self {
            lsof_program: "lsof".into(),
            ps_program: "ps".into(),
            tcp_timeout: LISTEN_PROBE_TIMEOUT,
            lsof_timeout: LISTEN_PROBE_TIMEOUT,
            lsof_kill_after: LISTEN_PROBE_KILL_AFTER,
        }
    }

    pub fn with_programs(
        lsof_program: impl Into<std::ffi::OsString>,
        ps_program: impl Into<std::ffi::OsString>,
    ) -> Self {
        Self {
            lsof_program: lsof_program.into(),
            ps_program: ps_program.into(),
            ..Self::new()
        }
    }
}

impl Default for SystemListenProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenProbe for SystemListenProbe {
    fn listening_pids(&self, port: u16) -> Result<Vec<u32>, ListenError> {
        let mut cmd = Command::new(&self.lsof_program);
        cmd.args(["-nP", "-a", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]);
        let output = launchd::run_with_deadline(
            &mut cmd,
            self.lsof_timeout,
            self.lsof_kill_after,
            LISTEN_POLL,
        )
        .map_err(|err| ListenError {
            message: format!("lsof: {err}"),
        })?;
        if output.timed_out {
            return Err(ListenError {
                message: "lsof timed out".into(),
            });
        }
        Ok(parse_lsof_pids(&String::from_utf8_lossy(&output.stdout)))
    }

    fn loopback_connects(&self, port: u16) -> bool {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&addr, self.tcp_timeout).is_ok()
    }

    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        let output = Command::new(&self.ps_program)
            .args(["-p", &pid.to_string(), "-o", "ppid=,comm="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_ps_identity(&String::from_utf8_lossy(&output.stdout))
    }
}

pub fn parse_lsof_pids(stdout: &str) -> Vec<u32> {
    stdout
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                trimmed.parse().ok()
            }
        })
        .collect()
}

pub fn parse_ps_identity(stdout: &str) -> Option<ProcessIdentity> {
    let line = stdout.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.split_whitespace();
    let ppid = parts.next().and_then(|tok| {
        if tok.bytes().all(|b| b.is_ascii_digit()) {
            tok.parse().ok()
        } else {
            None
        }
    });
    let comm = parts.next().unwrap_or("").to_string();
    Some(ProcessIdentity { ppid, comm })
}

pub fn lookup_mapping(mappings: &[Mapping], port: u16) -> Option<&Mapping> {
    mappings.iter().find(|mapping| mapping.local_port == port)
}

pub fn mappings_for_host<'a>(mappings: &'a [Mapping], canonical_host: &str) -> Vec<&'a Mapping> {
    mappings
        .iter()
        .filter(|mapping| mapping.host == canonical_host)
        .collect()
}

pub fn has_managed_tunnels(mappings: &[Mapping]) -> bool {
    !mappings.is_empty()
}

pub fn require_launchd_supervisor(supervisor: Supervisor) -> Result<(), TunnelError> {
    match supervisor {
        Supervisor::Launchd => Ok(()),
        Supervisor::None => Err(TunnelError::NoSupervisor),
    }
}

pub fn require_launchctl(backend: &impl Launchctl) -> Result<(), TunnelError> {
    if backend.is_available() {
        Ok(())
    } else {
        Err(TunnelError::LaunchctlRequired)
    }
}

/// Combine one job read with listener classification. Ownership uses the PID
/// captured here and must not query launchd again.
pub fn inspect(
    mapping: &Mapping,
    supervisor: Supervisor,
    uid: u32,
    launchd: &impl Launchctl,
    listen: &impl ListenProbe,
) -> Observation {
    let snapshot = supervisor_snapshot(supervisor, uid, &mapping.label, launchd);
    let listen = classify_local_listen(mapping.local_port, snapshot.pid, listen);
    Observation { snapshot, listen }
}

pub fn supervisor_snapshot(
    supervisor: Supervisor,
    uid: u32,
    label: &str,
    launchd: &impl Launchctl,
) -> JobSnapshot {
    if supervisor != Supervisor::Launchd {
        return JobSnapshot {
            state: SupervisorState::Unsupervised,
            loaded: Some(false),
            pid: None,
        };
    }

    let read = launchd::read_job(launchd, uid, label);
    if read.read == launchd::JobRead::Unknown || read.disabled.is_none() {
        return JobSnapshot {
            state: SupervisorState::Unknown,
            loaded: None,
            pid: read.pid,
        };
    }

    let loaded = match read.read {
        launchd::JobRead::Loaded => Some(true),
        launchd::JobRead::Missing => Some(false),
        launchd::JobRead::Unknown => None,
    };
    let mut state = if read.running {
        SupervisorState::Running
    } else {
        SupervisorState::Stopped
    };
    if read.disabled == Some(true) {
        state = SupervisorState::Paused;
    }
    JobSnapshot {
        state,
        loaded,
        pid: read.pid,
    }
}

/// Job PID or its direct `ssh` child owns the port. Grandchildren and
/// unrelated PIDs are `unrelated`. Failed inspection is `unknown`, not `none`.
pub fn classify_local_listen(
    port: u16,
    job_pid: Option<u32>,
    probe: &impl ListenProbe,
) -> ListenerOwnership {
    let pids = match probe.listening_pids(port) {
        Ok(pids) => pids,
        Err(_) => return ListenerOwnership::Unknown,
    };
    if pids.is_empty() {
        return if probe.loopback_connects(port) {
            ListenerOwnership::Unrelated
        } else {
            ListenerOwnership::None
        };
    }
    for listener in pids {
        if Some(listener) == job_pid {
            continue;
        }
        let Some(identity) = probe.process_identity(listener) else {
            return ListenerOwnership::Unrelated;
        };
        let is_direct_ssh = job_pid.is_some()
            && identity.ppid == job_pid
            && command_basename(&identity.comm) == "ssh";
        if !is_direct_ssh {
            return ListenerOwnership::Unrelated;
        }
    }
    ListenerOwnership::Owned
}

fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

pub fn print_status(
    out: &mut dyn Write,
    ctx: &TunnelContext<'_>,
    launchd: &impl Launchctl,
    listen: &impl ListenProbe,
) -> Result<(), TunnelError> {
    if ctx.supervisor == Supervisor::Launchd {
        require_launchctl(launchd)?;
    }
    writeln!(
        out,
        "{:<8} {:<28} {:<12} {:<10} LOCAL_LISTEN",
        "LOCAL", "REMOTE", "SUPERVISOR", "STATE"
    )
    .map_err(io_to_launchctl)?;
    let mut found = false;
    for mapping in ctx.mappings {
        found = true;
        let obs = inspect(mapping, ctx.supervisor, ctx.uid, launchd, listen);
        writeln!(
            out,
            "{:<8} {:<28} {:<12} {:<10} {}",
            mapping.local_port,
            mapping.remote_display(),
            ctx.supervisor.as_str(),
            obs.state().as_str(),
            obs.listen.as_str()
        )
        .map_err(io_to_launchctl)?;
    }
    if !found {
        writeln!(out, "{NO_TUNNELS_CONFIGURED}").map_err(io_to_launchctl)?;
    } else if ctx.supervisor != Supervisor::Launchd {
        writeln!(out, "{MACOS_ONLY_MESSAGE}").map_err(io_to_launchctl)?;
    }
    Ok(())
}

pub fn pause(
    port: u16,
    ctx: &TunnelContext<'_>,
    launchd: &impl Launchctl,
) -> Result<String, TunnelError> {
    require_launchd_supervisor(ctx.supervisor)?;
    require_launchctl(launchd)?;
    let mapping = lookup_mapping(ctx.mappings, port).ok_or(TunnelError::NoMapping { port })?;
    let target = gui_target(ctx.uid, &mapping.label);
    if !launchd::launchctl_disable(launchd, &target).success() {
        return Err(TunnelError::DisableFailed { target });
    }
    match job_is_loaded(launchd, &target) {
        Ok(true) => {
            if !launchd::launchctl_bootout_wait(launchd, &target).success() {
                return Err(TunnelError::BootoutFailed { target });
            }
        }
        Ok(false) => {}
        Err(_) => return Err(TunnelError::JobStateUnknown { target }),
    }
    Ok(format!(
        "fleet: paused managed tunnel for local port {port}\n"
    ))
}

pub fn resume(
    port: u16,
    ctx: &TunnelContext<'_>,
    launchd: &impl Launchctl,
    listen: &impl ListenProbe,
) -> Result<String, TunnelError> {
    require_launchd_supervisor(ctx.supervisor)?;
    require_launchctl(launchd)?;
    let mapping = lookup_mapping(ctx.mappings, port).ok_or(TunnelError::NoMapping { port })?;
    let target = gui_target(ctx.uid, &mapping.label);
    let obs = inspect(mapping, ctx.supervisor, ctx.uid, launchd, listen);

    if obs.state() == SupervisorState::Unknown {
        return Err(TunnelError::JobStateUnknown { target });
    }
    if obs.state() == SupervisorState::Running && obs.listen == ListenerOwnership::Owned {
        return Ok(format!(
            "fleet: managed tunnel for local port {port} is already running\n"
        ));
    }
    if obs.listen == ListenerOwnership::Unrelated {
        return Err(TunnelError::PortOccupied { port });
    }
    if obs.listen == ListenerOwnership::Unknown {
        return Err(TunnelError::ListenerUnknown { port });
    }

    let plist = launch_agent_plist(ctx.home, &mapping.label);
    if !plist.is_file() {
        return Err(TunnelError::PlistMissing { path: plist });
    }

    if !launchd::launchctl_enable(launchd, &target).success() {
        return Err(TunnelError::EnableFailed { target });
    }

    let mut snap = supervisor_snapshot(ctx.supervisor, ctx.uid, &mapping.label, launchd);
    if snap.state == SupervisorState::Unknown {
        return Err(TunnelError::JobStateUnknown { target });
    }
    if snap.loaded != Some(true) {
        if !launchd::launchctl_bootstrap(launchd, &launchd::gui_domain(ctx.uid), &plist).success() {
            return Err(TunnelError::BootstrapFailed { target });
        }
        snap = supervisor_snapshot(ctx.supervisor, ctx.uid, &mapping.label, launchd);
        if snap.state == SupervisorState::Unknown {
            return Err(TunnelError::JobStateUnknown { target });
        }
    }
    if snap.state != SupervisorState::Running
        && !launchd::launchctl_kickstart(launchd, &target).success()
    {
        return Err(TunnelError::KickstartFailed { target });
    }
    Ok(format!(
        "fleet: resumed managed tunnel for local port {port}\n"
    ))
}

/// Classify a discovered SSH-forward PID for `fleet forward delete`.
///
/// Supervisor `none` never fabricates ownership. A paused mapping does not
/// make an unrelated occupant managed. If a launchd mapping might own the
/// candidate but the snapshot cannot be read, the verdict is ambiguous.
pub fn managed_delete_verdict(
    pid: u32,
    local_port: u16,
    ctx: &TunnelContext<'_>,
    launchd: &impl Launchctl,
    listen: &impl ListenProbe,
) -> ManagedDeleteVerdict {
    let Some(mapping) = lookup_mapping(ctx.mappings, local_port) else {
        return ManagedDeleteVerdict::NotManaged;
    };
    if ctx.supervisor != Supervisor::Launchd {
        return ManagedDeleteVerdict::NotManaged;
    }
    if !launchd.is_available() {
        return ManagedDeleteVerdict::Ambiguous { port: local_port };
    }
    let obs = inspect(mapping, ctx.supervisor, ctx.uid, launchd, listen);
    if obs.state() == SupervisorState::Unknown || obs.listen == ListenerOwnership::Unknown {
        return ManagedDeleteVerdict::Ambiguous { port: local_port };
    }
    if obs.state() == SupervisorState::Paused || obs.state() == SupervisorState::Stopped {
        return ManagedDeleteVerdict::NotManaged;
    }
    if obs.snapshot.pid == Some(pid) {
        return ManagedDeleteVerdict::Managed { port: local_port };
    }
    if obs.listen == ListenerOwnership::Owned
        || (obs.state() == SupervisorState::Running && obs.snapshot.pid.is_some())
    {
        return match listener_is_pid_or_direct_ssh(pid, obs.snapshot.pid, listen) {
            Some(true) => ManagedDeleteVerdict::Managed { port: local_port },
            Some(false) => ManagedDeleteVerdict::NotManaged,
            None => ManagedDeleteVerdict::Ambiguous { port: local_port },
        };
    }
    ManagedDeleteVerdict::NotManaged
}

fn listener_is_pid_or_direct_ssh(
    pid: u32,
    job_pid: Option<u32>,
    probe: &impl ListenProbe,
) -> Option<bool> {
    if Some(pid) == job_pid {
        return Some(true);
    }
    probe.process_identity(pid).map(|identity| {
        job_pid.is_some() && identity.ppid == job_pid && command_basename(&identity.comm) == "ssh"
    })
}

fn io_to_launchctl(err: io::Error) -> TunnelError {
    TunnelError::JobStateUnknown {
        target: err.to_string(),
    }
}
