//! Process lookup, argv planning, Unix exec replacement, and bounded
//! subprocess runs.
//!
//! Production selects `ssh`, `tmux`, `ps`, and `kill` from PATH. Tests inject
//! fake executables by putting them first on PATH. Fleet itself does not read
//! fixture-only environment switches to choose those programs.

use std::convert::Infallible;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

const SIGTERM: i32 = 15;
const TIMEOUT_EXIT: i32 = 124;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Environment values Fleet reads. Tests construct this directly; the CLI
/// fills it from the real process environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessEnv {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub fleet_config: Option<PathBuf>,
    pub shell: Option<OsString>,
    pub path: Option<OsString>,
}

impl ProcessEnv {
    pub fn from_os() -> Self {
        Self {
            home: std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            fleet_config: std::env::var_os("FLEET_CONFIG")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            shell: std::env::var_os("SHELL").filter(|v| !v.is_empty()),
            path: std::env::var_os("PATH"),
        }
    }

    pub fn shell_program(&self) -> String {
        self.shell
            .as_ref()
            .and_then(|s| s.to_str().map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string())
    }
}

/// External program Fleet will exec or spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Short description used when the program is missing from PATH.
    pub operation: &'static str,
}

impl PlannedCommand {
    pub fn ssh(args: Vec<String>, operation: &'static str) -> Self {
        Self {
            program: "ssh".into(),
            args,
            operation,
        }
    }

    pub fn tmux(args: Vec<String>) -> Self {
        Self {
            program: "tmux".into(),
            args,
            operation: "attach a local tmux session",
        }
    }

    pub fn program(program: String, args: Vec<String>, operation: &'static str) -> Self {
        Self {
            program,
            args,
            operation,
        }
    }
}

/// Observed process table row after splitting `ps` output on whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedProcess {
    pub pid: u32,
    pub argv: Vec<String>,
}

pub trait ProcessTable {
    fn list(&self) -> Result<Vec<ObservedProcess>, ProcessError>;
}

/// Live `ps` lookup. Tries `ps axww -o pid=,command=` then `ps -eww -o pid=,args=`.
pub struct PathPsTable<'a> {
    pub env: &'a ProcessEnv,
}

impl ProcessTable for PathPsTable<'_> {
    fn list(&self) -> Result<Vec<ObservedProcess>, ProcessError> {
        match run_ps(self.env, &["axww", "-o", "pid=,command="]) {
            Ok(text) => Ok(parse_ps_output(&text)),
            Err(first_error) => match run_ps(self.env, &["-eww", "-o", "pid=,args="]) {
                Ok(text) => Ok(parse_ps_output(&text)),
                Err(ProcessError::MissingCommand { .. }) => Err(first_error),
                Err(second_error) => Err(second_error),
            },
        }
    }
}

pub trait SignalSender {
    fn signal(&self, pid: u32) -> Result<(), ProcessError>;
}

/// Sends `kill PID` through PATH so tests can inject a fake `kill`.
pub struct PathKill<'a> {
    pub env: &'a ProcessEnv,
}

impl SignalSender for PathKill<'_> {
    fn signal(&self, pid: u32) -> Result<(), ProcessError> {
        let mut cmd = Command::new("kill");
        apply_path(&mut cmd, self.env);
        cmd.arg(pid.to_string());
        match cmd.status() {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(ProcessError::KillFailed { pid }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(ProcessError::MissingCommand {
                    name: "kill".into(),
                    operation: "stop an SSH forward".into(),
                })
            }
            Err(source) => Err(ProcessError::Spawn {
                name: "kill".into(),
                source,
            }),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("fleet: missing command `{name}` (required to {operation})")]
    MissingCommand { name: String, operation: String },
    #[error("fleet: failed to run `{name}`: {source}")]
    Spawn { name: String, source: io::Error },
    #[error("fleet: failed to stop SSH forward process {pid}")]
    KillFailed { pid: u32 },
    #[error("fleet: failed to exec `{name}`: {source}")]
    Exec { name: String, source: io::Error },
}

impl ProcessError {
    pub fn exit_code(&self) -> i32 {
        1
    }
}

/// Replace this process with `planned`. Returns only if exec fails.
pub fn exec_replace(planned: &PlannedCommand) -> Result<(), ProcessError> {
    let mut cmd = Command::new(&planned.program);
    cmd.args(&planned.args);
    let error = cmd.exec();
    Err(map_exec_error(&planned.program, planned.operation, error))
}

fn map_exec_error(name: &str, operation: &str, error: io::Error) -> ProcessError {
    if error.kind() == io::ErrorKind::NotFound {
        ProcessError::MissingCommand {
            name: name.to_string(),
            operation: operation.to_string(),
        }
    } else {
        ProcessError::Exec {
            name: name.to_string(),
            source: error,
        }
    }
}

/// Split `ps` lines the way the legacy script word-split them.
pub fn parse_ps_output(text: &str) -> Vec<ObservedProcess> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let argv: Vec<String> = fields.map(str::to_string).collect();
            if argv.is_empty() {
                return None;
            }
            Some(ObservedProcess { pid, argv })
        })
        .collect()
}

fn run_ps(env: &ProcessEnv, args: &[&str]) -> Result<String, ProcessError> {
    let mut cmd = Command::new("ps");
    apply_path(&mut cmd, env);
    cmd.args(args);
    let output = cmd.output().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessError::MissingCommand {
                name: "ps".into(),
                operation: "list SSH forwards".into(),
            }
        } else {
            ProcessError::Spawn {
                name: "ps".into(),
                source,
            }
        }
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(ProcessError::Spawn {
            name: "ps".into(),
            source: io::Error::other("ps exited unsuccessfully"),
        })
    }
}

fn apply_path(cmd: &mut Command, env: &ProcessEnv) {
    if let Some(path) = &env.path {
        cmd.env("PATH", path);
    }
}

#[derive(Debug)]
pub struct BoundedOutput {
    pub timed_out: bool,
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// GNU `timeout --kill-after` analogue: SIGTERM at `timeout`, SIGKILL after
/// `kill_after`. Timed-out commands report exit 124. Used by doctor SSH and
/// listener `lsof`.
pub(crate) fn run_with_deadline(
    cmd: &mut Command,
    timeout: Duration,
    kill_after: Duration,
    poll: Duration,
) -> io::Result<BoundedOutput> {
    match run_with_deadline_or_stop(cmd, timeout, kill_after, poll, || None::<Infallible>)? {
        Ok(output) => Ok(output),
        Err(never) => match never {},
    }
}

/// [`run_with_deadline`] that also polls `stop` before each wait. When `stop`
/// returns `Some(reason)`, the child is killed and reaped and the reason is
/// returned as `Ok(Err(reason))`; its partial output is discarded. The
/// runner's startup probe uses this to honor SIGINT/SIGTERM mid-probe.
pub(crate) fn run_with_deadline_or_stop<S>(
    cmd: &mut Command,
    timeout: Duration,
    kill_after: Duration,
    poll: Duration,
    stop: impl Fn() -> Option<S>,
) -> io::Result<Result<BoundedOutput, S>> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = drain(child.stdout.take(), "stdout")?;
    let stderr = drain(child.stderr.take(), "stderr")?;
    let poll = poll.max(Duration::from_millis(1));

    let finish = match wait_bounded(&mut child, timeout, kill_after, poll, &stop) {
        Ok(Ok(finish)) => finish,
        Ok(Err(reason)) => {
            kill_and_reap(&mut child);
            return Ok(Err(reason));
        }
        Err(error) => {
            kill_and_reap(&mut child);
            return Err(error);
        }
    };
    if finish == Finish::TimedOut(None) {
        kill_and_reap(&mut child);
    }

    let stdout = stdout.join().unwrap_or_default();
    let mut stderr = stderr.join().unwrap_or_default();
    Ok(Ok(match finish {
        Finish::Exited(status) => BoundedOutput {
            timed_out: false,
            code: exit_status_code(status),
            stdout,
            stderr,
        },
        Finish::TimedOut(exit) => {
            if let (true, Some(status)) = (stderr.is_empty(), exit) {
                stderr = format!("timed out; child exited {status}").into_bytes();
            }
            BoundedOutput {
                timed_out: true,
                code: TIMEOUT_EXIT,
                stdout,
                stderr,
            }
        }
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    Exited(ExitStatus),
    /// Past the deadline. `Some` if the child exited during the SIGTERM grace
    /// period; `None` if it still needs SIGKILL.
    TimedOut(Option<ExitStatus>),
}

enum Wait<S> {
    Exited(ExitStatus),
    Expired,
    Stopped(S),
}

/// Wait for exit until `timeout`, then SIGTERM and wait `kill_after` more.
/// `Err(reason)` as soon as `stop` asks for it.
fn wait_bounded<S>(
    child: &mut Child,
    timeout: Duration,
    kill_after: Duration,
    poll: Duration,
    stop: &impl Fn() -> Option<S>,
) -> io::Result<Result<Finish, S>> {
    match wait_until(child, Instant::now() + timeout, poll, stop)? {
        Wait::Exited(status) => return Ok(Ok(Finish::Exited(status))),
        Wait::Stopped(reason) => return Ok(Err(reason)),
        Wait::Expired => {}
    }
    send_signal(child.id(), SIGTERM);
    Ok(
        match wait_until(child, Instant::now() + kill_after, poll, stop)? {
            Wait::Exited(status) => Ok(Finish::TimedOut(Some(status))),
            Wait::Expired => Ok(Finish::TimedOut(None)),
            Wait::Stopped(reason) => Err(reason),
        },
    )
}

fn wait_until<S>(
    child: &mut Child,
    until: Instant,
    poll: Duration,
    stop: &impl Fn() -> Option<S>,
) -> io::Result<Wait<S>> {
    loop {
        if let Some(reason) = stop() {
            return Ok(Wait::Stopped(reason));
        }
        if let Some(status) = child.try_wait()? {
            return Ok(Wait::Exited(status));
        }
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(Wait::Expired);
        }
        thread::sleep(poll.min(remaining));
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drain(
    pipe: Option<impl Read + Send + 'static>,
    name: &str,
) -> io::Result<thread::JoinHandle<Vec<u8>>> {
    let mut pipe = pipe.ok_or_else(|| io::Error::other(format!("missing {name} pipe")))?;
    Ok(thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    }))
}

/// Signal one positive PID. PIDs that do not fit a positive `pid_t` are
/// ignored: `kill` treats zero and negative values as process groups.
pub(crate) fn send_signal(pid: u32, sig: i32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    if pid <= 0 {
        return;
    }
    // SAFETY: kill has no memory-safety preconditions; ESRCH is ignored.
    unsafe {
        let _ = kill(pid, sig);
    }
}

pub(crate) fn exit_status_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|sig| 128 + sig))
        .unwrap_or(1)
}

/// True when `program` resolves to an executable file, directly or on PATH.
pub(crate) fn program_is_runnable(program: &OsStr) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return is_executable_file(path);
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| is_executable_file(&dir.join(path)))
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}
