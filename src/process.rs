//! Process lookup, argv planning, and Unix exec replacement.
//!
//! Production selects `ssh`, `tmux`, `ps`, and `kill` from PATH. Tests inject
//! fake executables by putting them first on PATH. Fleet itself does not read
//! fixture-only environment switches to choose those programs.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

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

/// Fixed process snapshot for tests. `list` always returns a clone of `rows`.
#[derive(Debug, Clone, Default)]
pub struct SnapshotProcesses {
    pub rows: Vec<ObservedProcess>,
}

impl ProcessTable for SnapshotProcesses {
    fn list(&self) -> Result<Vec<ObservedProcess>, ProcessError> {
        Ok(self.rows.clone())
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

/// Records signaled PIDs instead of sending a real signal.
#[derive(Debug, Default)]
pub struct RecordingSignals {
    pub pids: std::sync::Mutex<Vec<u32>>,
}

impl SignalSender for RecordingSignals {
    fn signal(&self, pid: u32) -> Result<(), ProcessError> {
        self.pids.lock().expect("signal log mutex").push(pid);
        Ok(())
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

/// True when `path` is an executable regular file. Used by tests.
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    meta.is_file() && meta.permissions().mode() & 0o111 != 0
}
