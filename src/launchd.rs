//! launchd job identifiers, `launchctl` parsing, and local subprocess deadlines.
//!
//! Pause intent is a persistent launchd override against the existing job
//! label `org.nix-community.home.fleet-tunnel-PORT`. Do not rename labels:
//! that would drop pause state stored outside the plist.
//!
//! Job state and PID come from one `launchctl print` read. Disabled-service
//! status is a second read, matching launchd rather than inventing an atomic
//! OS snapshot.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Label prefix used by Home Manager for managed Fleet tunnel jobs.
pub const FLEET_TUNNEL_LABEL_PREFIX: &str = "org.nix-community.home.fleet-tunnel-";

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const TIMEOUT_EXIT: i32 = 124;

extern "C" {
    fn getuid() -> u32;
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Current process uid, used to build `gui/UID` launchd domains.
pub fn current_uid() -> u32 {
    // SAFETY: getuid is always successful and has no preconditions.
    unsafe { getuid() }
}

/// `org.nix-community.home.fleet-tunnel-PORT`
pub fn tunnel_label(port: u16) -> String {
    format!("{FLEET_TUNNEL_LABEL_PREFIX}{port}")
}

/// `gui/UID`
pub fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// `gui/UID/LABEL`
pub fn gui_target(uid: u32, label: &str) -> String {
    format!("gui/{uid}/{label}")
}

/// `$HOME/Library/LaunchAgents/LABEL.plist`
pub fn launch_agent_plist(home: &Path, label: &str) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

/// Outcome of one `launchctl` invocation.
#[derive(Debug)]
pub enum LaunchctlOutput {
    Ran {
        code: i32,
        stdout: String,
        stderr: String,
    },
    Spawn(io::Error),
}

impl LaunchctlOutput {
    pub fn success(&self) -> bool {
        matches!(self, Self::Ran { code: 0, .. })
    }

    pub fn stdout(&self) -> &str {
        match self {
            Self::Ran { stdout, .. } => stdout,
            Self::Spawn(_) => "",
        }
    }

    pub fn stderr(&self) -> &str {
        match self {
            Self::Ran { stderr, .. } => stderr,
            Self::Spawn(_) => "",
        }
    }

    fn spawn_failed(&self) -> bool {
        matches!(self, Self::Spawn(_))
    }

    fn could_not_find_service(&self) -> bool {
        match self {
            Self::Ran { code, stderr, .. } if *code != 0 => {
                stderr.contains("Could not find service")
            }
            _ => false,
        }
    }
}

/// Seam for `launchctl`. Tests inject a fake; production uses [`SystemLaunchctl`].
pub trait Launchctl {
    fn run(&self, args: &[OsString]) -> LaunchctlOutput;

    fn is_available(&self) -> bool {
        true
    }
}

impl<T: Launchctl + ?Sized> Launchctl for &T {
    fn run(&self, args: &[OsString]) -> LaunchctlOutput {
        (**self).run(args)
    }

    fn is_available(&self) -> bool {
        (**self).is_available()
    }
}

/// Process PATH `launchctl`. Never used by tests against a live Darwin daemon.
pub struct SystemLaunchctl {
    program: OsString,
}

impl SystemLaunchctl {
    pub fn new() -> Self {
        Self {
            program: OsString::from("launchctl"),
        }
    }

    pub fn with_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for SystemLaunchctl {
    fn default() -> Self {
        Self::new()
    }
}

impl Launchctl for SystemLaunchctl {
    fn run(&self, args: &[OsString]) -> LaunchctlOutput {
        match Command::new(&self.program).args(args).output() {
            Ok(output) => LaunchctlOutput::Ran {
                code: exit_status_code(output.status),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(err) => LaunchctlOutput::Spawn(err),
        }
    }

    fn is_available(&self) -> bool {
        program_is_runnable(&self.program)
    }
}

pub fn launchctl_print(backend: &impl Launchctl, target: &str) -> LaunchctlOutput {
    backend.run(&[OsString::from("print"), OsString::from(target)])
}

pub fn launchctl_print_disabled(backend: &impl Launchctl, domain: &str) -> LaunchctlOutput {
    backend.run(&[OsString::from("print-disabled"), OsString::from(domain)])
}

pub fn launchctl_disable(backend: &impl Launchctl, target: &str) -> LaunchctlOutput {
    backend.run(&[OsString::from("disable"), OsString::from(target)])
}

pub fn launchctl_enable(backend: &impl Launchctl, target: &str) -> LaunchctlOutput {
    backend.run(&[OsString::from("enable"), OsString::from(target)])
}

pub fn launchctl_bootout_wait(backend: &impl Launchctl, target: &str) -> LaunchctlOutput {
    backend.run(&[
        OsString::from("bootout"),
        OsString::from("--wait"),
        OsString::from(target),
    ])
}

pub fn launchctl_bootstrap(
    backend: &impl Launchctl,
    domain: &str,
    plist: &Path,
) -> LaunchctlOutput {
    backend.run(&[
        OsString::from("bootstrap"),
        OsString::from(domain),
        plist.as_os_str().to_os_string(),
    ])
}

pub fn launchctl_kickstart(backend: &impl Launchctl, target: &str) -> LaunchctlOutput {
    backend.run(&[OsString::from("kickstart"), OsString::from(target)])
}

/// Fields taken from one `launchctl print` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintFields {
    pub running: bool,
    pub pid: Option<u32>,
}

/// Parse `launchctl print` text. Only `state = running` counts as running;
/// `state = not running` stays stopped. The last numeric `pid = N` wins.
pub fn parse_launchctl_print(output: &str) -> PrintFields {
    let mut running = false;
    let mut pid = None;
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(equals) = parts.next() else {
            continue;
        };
        if equals != "=" {
            continue;
        }
        let Some(value) = parts.next() else {
            continue;
        };
        match key {
            "state" if value == "running" => {
                running = true;
            }
            "pid" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                if let Ok(parsed) = value.parse::<u32>() {
                    pid = Some(parsed);
                }
            }
            _ => {}
        }
    }
    PrintFields { running, pid }
}

/// Labels launchd reports as disabled. Keys must be quoted in the output so
/// `org.nix-community.home.fleet-tunnel-30000` cannot pause port 3000.
pub fn parse_print_disabled(output: &str) -> HashSet<String> {
    let mut labels = HashSet::new();
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(arrow) = parts.next() else {
            continue;
        };
        if arrow != "=>" {
            continue;
        }
        let Some(value) = parts.next() else {
            continue;
        };
        let Some(label) = quoted_label(key) else {
            continue;
        };
        if value == "true" || value == "disabled" {
            labels.insert(label);
        }
    }
    labels
}

fn quoted_label(key: &str) -> Option<String> {
    let inner = key.strip_prefix('"')?.strip_suffix('"')?;
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// How one job observation combined print + print-disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobRead {
    /// `launchctl print` found the job.
    Loaded,
    /// `Could not find service` (or equivalent missing-job failure).
    Missing,
    /// spawn failure or unexpected print status; do not treat as absence.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedJobRead {
    pub read: JobRead,
    pub running: bool,
    pub pid: Option<u32>,
    pub disabled: Option<bool>,
}

/// One print of the job plus one print-disabled of the gui domain.
///
/// `disabled` is `None` when print-disabled cannot be trusted. Callers must
/// not interpret that as "not paused".
pub fn read_job(backend: &impl Launchctl, uid: u32, label: &str) -> CombinedJobRead {
    let print = launchctl_print(backend, &gui_target(uid, label));
    let (read, running, pid) = match &print {
        LaunchctlOutput::Spawn(_) => (JobRead::Unknown, false, None),
        LaunchctlOutput::Ran {
            code: 0, stdout, ..
        } => {
            let fields = parse_launchctl_print(stdout);
            (JobRead::Loaded, fields.running, fields.pid)
        }
        LaunchctlOutput::Ran { .. } if print.could_not_find_service() => {
            (JobRead::Missing, false, None)
        }
        LaunchctlOutput::Ran { .. } => (JobRead::Unknown, false, None),
    };

    let disabled_out = launchctl_print_disabled(backend, &gui_domain(uid));
    let disabled = match disabled_out {
        LaunchctlOutput::Ran {
            code: 0, stdout, ..
        } => Some(parse_print_disabled(&stdout).contains(label)),
        LaunchctlOutput::Ran { .. } | LaunchctlOutput::Spawn(_) => None,
    };

    CombinedJobRead {
        read,
        running,
        pid,
        disabled,
    }
}

pub fn job_is_loaded(backend: &impl Launchctl, target: &str) -> Result<bool, JobRead> {
    let print = launchctl_print(backend, target);
    if print.spawn_failed() {
        return Err(JobRead::Unknown);
    }
    if print.success() {
        return Ok(true);
    }
    if print.could_not_find_service() {
        return Ok(false);
    }
    Err(JobRead::Unknown)
}

#[derive(Debug)]
pub struct BoundedOutput {
    pub timed_out: bool,
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// GNU `timeout --kill-after` analogue: SIGTERM at `timeout`, SIGKILL after
/// `kill_after`. Timed-out commands report exit 124. Used by doctor SSH,
/// listener `lsof`, and the runner's startup probe.
pub fn run_with_deadline(
    cmd: &mut Command,
    timeout: Duration,
    kill_after: Duration,
    poll: Duration,
) -> io::Result<BoundedOutput> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout pipe"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr pipe"))?;
    let stdout_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let poll = poll.max(Duration::from_millis(1));
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = stdout_thread.join().unwrap_or_default();
            let stderr = stderr_thread.join().unwrap_or_default();
            return Ok(BoundedOutput {
                timed_out: false,
                code: exit_status_code(status),
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
    }

    send_signal(child.id(), SIGTERM);
    let kill_at = Instant::now() + kill_after;
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = stdout_thread.join().unwrap_or_default();
            let stderr = stderr_thread.join().unwrap_or_default();
            return Ok(BoundedOutput {
                timed_out: true,
                code: TIMEOUT_EXIT,
                stdout,
                stderr: if stderr.is_empty() {
                    format!("timed out; child exited {status}").into_bytes()
                } else {
                    stderr
                },
            });
        }
        if Instant::now() >= kill_at {
            send_signal(child.id(), SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        thread::sleep(poll);
    }

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(BoundedOutput {
        timed_out: true,
        code: TIMEOUT_EXIT,
        stdout,
        stderr,
    })
}

pub(crate) fn send_signal(pid: u32, sig: i32) {
    // SAFETY: kill(pid, sig) is valid for any pid; ESRCH is ignored.
    unsafe {
        let _ = kill(pid as i32, sig);
    }
}

pub(crate) fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

pub(crate) fn program_is_runnable(program: &OsStr) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return is_executable_file(path);
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&paths) {
        if is_executable_file(&dir.join(path)) {
            return true;
        }
    }
    false
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match path.metadata() {
            Ok(meta) => meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        true
    }
}
