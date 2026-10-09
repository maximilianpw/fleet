//! Bounded, non-interactive commands on Fleet hosts for the multi-host query
//! commands (`status`, `agents`, `ports`, `move`, and doctor's tool checks).
//!
//! Scripts are POSIX `sh`. Remote login shells may be fish, so a script is
//! wrapped as one `sh -c '...'` argument that both POSIX shells and fish parse
//! identically. That only holds while the wrapped text has no backslashes,
//! which is why [`validate_script_value`] rejects them in interpolated values.
//!
//! These queries share one SSH connection per host through a Fleet-owned
//! `ControlPath`. Existing commands (`ssh`, `shell`, `run`, `copy`, forwards,
//! tunnels, doctor reachability) keep their own argv and never use it.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::process::{run_group_with_deadline, run_with_deadline, ProcessEnv};

pub const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
pub const QUERY_KILL_AFTER: Duration = Duration::from_secs(2);
pub const QUERY_POLL: Duration = Duration::from_millis(20);
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: &str = "ConnectTimeout=8";
const CONTROL_PERSIST: &str = "ControlPersist=60";

/// Extra directories appended to PATH on the far side. Non-interactive SSH
/// sessions often skip the profile that puts Nix and Homebrew tools on PATH.
pub const REMOTE_PATH_SETUP: &str = "PATH=\"$PATH:$HOME/.nix-profile/bin:/etc/profiles/per-user/$USER/bin:/run/current-system/sw/bin:/nix/var/nix/profiles/default/bin:/opt/homebrew/bin:/usr/local/bin\"; export PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Local,
    Ssh { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl QueryOutput {
    pub fn success(&self) -> bool {
        self.code == 0 && !self.timed_out
    }

    fn spawn_failure(name: &str, error: &io::Error) -> Self {
        Self {
            code: 127,
            stdout: String::new(),
            stderr: format!("failed to run {name}: {error}"),
            timed_out: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum RemoteError {
    #[error("fleet: value contains a backslash or control character and cannot be sent to a remote shell: {0}")]
    UnsafeValue(String),
    #[error("fleet: transfer failed: {0}")]
    Transfer(String),
}

impl RemoteError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UnsafeValue(_) => 2,
            Self::Transfer(_) => 1,
        }
    }
}

/// Runs scripts on the local machine or over SSH with connection reuse.
#[derive(Debug, Clone)]
pub struct Runner {
    control_dir: Option<PathBuf>,
    timeout: Duration,
}

impl Runner {
    pub fn new(env: &ProcessEnv) -> Self {
        Self {
            control_dir: prepare_control_dir(env),
            timeout: QUERY_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run `script` with `sh -c` locally or on the remote host, bounded by
    /// the runner's timeout. Never returns an error: spawn failures and
    /// timeouts are reported in [`QueryOutput`].
    pub fn run(&self, endpoint: &Endpoint, script: &str) -> QueryOutput {
        let mut cmd = self.command(endpoint, script, false);
        let name = program_name(endpoint);
        let result = match endpoint {
            Endpoint::Local => {
                run_group_with_deadline(&mut cmd, self.timeout, QUERY_KILL_AFTER, QUERY_POLL)
            }
            Endpoint::Ssh { .. } => {
                run_with_deadline(&mut cmd, self.timeout, QUERY_KILL_AFTER, QUERY_POLL)
            }
        };
        match result {
            Ok(output) => QueryOutput {
                code: output.code,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                timed_out: output.timed_out,
            },
            Err(error) => QueryOutput::spawn_failure(name, &error),
        }
    }

    /// Stream the stdout of `source_script` on `source` into the stdin of
    /// `dest_script` on `dest`, through this machine.
    ///
    /// If either side fails, the other is killed at once, so a dead writer
    /// cannot leave the reader blocked on a full pipe until the deadline.
    pub fn pipe(
        &self,
        source: &Endpoint,
        source_script: &str,
        dest: &Endpoint,
        dest_script: &str,
        timeout: Duration,
    ) -> Result<(), RemoteError> {
        let mut producer = {
            let mut cmd = self.command(source, source_script, false);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            cmd.spawn().map_err(|error| {
                RemoteError::Transfer(format!("failed to start reader: {error}"))
            })?
        };
        let stdout = producer
            .stdout
            .take()
            .ok_or_else(|| RemoteError::Transfer("reader has no stdout".into()))?;
        // The Command owning the pipe's read end is dropped at the end of this
        // block, so only the writer holds it and a dead writer means SIGPIPE.
        let consumer = {
            let mut cmd = self.command(dest, dest_script, true);
            cmd.stdin(Stdio::from(stdout))
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            cmd.spawn()
        };
        let mut consumer = match consumer {
            Ok(child) => child,
            Err(error) => {
                let _ = producer.kill();
                let _ = producer.wait();
                return Err(RemoteError::Transfer(format!(
                    "failed to start writer: {error}"
                )));
            }
        };
        let producer_err = drain_stderr(&mut producer);
        let consumer_err = drain_stderr(&mut consumer);

        let deadline = Instant::now() + timeout;
        let mut producer_status = None;
        let mut consumer_status = None;
        let mut timed_out = false;
        while producer_status.is_none() || consumer_status.is_none() {
            if producer_status.is_none() {
                producer_status = producer.try_wait().ok().flatten();
            }
            if consumer_status.is_none() {
                consumer_status = consumer.try_wait().ok().flatten();
            }
            let failed = |status: Option<std::process::ExitStatus>| {
                status.is_some_and(|status| !status.success())
            };
            if failed(producer_status) && consumer_status.is_none() {
                let _ = consumer.kill();
            }
            if failed(consumer_status) && producer_status.is_none() {
                let _ = producer.kill();
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = producer.kill();
                let _ = consumer.kill();
                producer_status = producer.wait().ok();
                consumer_status = consumer.wait().ok();
                break;
            }
            thread::sleep(QUERY_POLL);
        }
        let producer_err = producer_err.join().unwrap_or_default();
        let consumer_err = consumer_err.join().unwrap_or_default();
        if timed_out {
            return Err(RemoteError::Transfer("timed out".into()));
        }
        if !consumer_status.is_some_and(|status| status.success()) {
            return Err(RemoteError::Transfer(format!(
                "writing the destination failed: {}",
                consumer_err.trim()
            )));
        }
        if !producer_status.is_some_and(|status| status.success()) {
            return Err(RemoteError::Transfer(format!(
                "reading the source failed: {}",
                producer_err.trim()
            )));
        }
        Ok(())
    }

    fn command(&self, endpoint: &Endpoint, script: &str, keep_stdin: bool) -> Command {
        match endpoint {
            Endpoint::Local => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(script);
                cmd
            }
            Endpoint::Ssh { target } => {
                let mut cmd = Command::new("ssh");
                cmd.args(query_ssh_argv(
                    target,
                    &wrap_for_login_shell(script),
                    self.control_dir.as_ref(),
                    keep_stdin,
                ));
                cmd
            }
        }
    }
}

fn drain_stderr(child: &mut std::process::Child) -> thread::JoinHandle<String> {
    let pipe = child.stderr.take();
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = io::Read::read_to_string(&mut pipe, &mut text);
        }
        text
    })
}

fn program_name(endpoint: &Endpoint) -> &'static str {
    match endpoint {
        Endpoint::Local => "sh",
        Endpoint::Ssh { .. } => "ssh",
    }
}

/// OpenSSH argv for a query. With a control directory, the first query to a
/// host opens a background master that later queries reuse for 60 seconds.
pub fn query_ssh_argv(
    target: &str,
    remote_command: &str,
    control_dir: Option<&PathBuf>,
    keep_stdin: bool,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if !keep_stdin {
        args.push("-n".into());
    }
    for option in ["BatchMode=yes", CONNECT_TIMEOUT, "ForwardAgent=no"] {
        args.push("-o".into());
        args.push(option.into());
    }
    match control_dir {
        Some(dir) => {
            let mut path = OsString::from("ControlPath=");
            path.push(dir.join("%C"));
            for option in [
                OsString::from("ControlMaster=auto"),
                path,
                CONTROL_PERSIST.into(),
            ] {
                args.push("-o".into());
                args.push(option);
            }
        }
        None => {
            for option in ["ControlMaster=no", "ControlPath=none"] {
                args.push("-o".into());
                args.push(option.into());
            }
        }
    }
    args.push(target.into());
    args.push(remote_command.into());
    args
}

/// Wrap a POSIX script as one `sh -c '...'` word for a POSIX or fish login
/// shell. Single quotes become `'\''`, which both shells read as a literal
/// quote between two quoted segments.
pub fn wrap_for_login_shell(script: &str) -> String {
    debug_assert!(
        !script.contains('\\'),
        "remote scripts must not contain backslashes"
    );
    format!("sh -c '{}'", script.replace('\'', "'\\''"))
}

/// POSIX single-quoted literal for interpolation into a script.
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Values interpolated into remote scripts must not contain backslashes
/// (fish treats `\\` and `\'` specially inside single quotes) or control
/// characters.
pub fn validate_script_value(value: &str) -> Result<(), RemoteError> {
    if value.contains('\\') || value.chars().any(char::is_control) {
        Err(RemoteError::UnsafeValue(value.to_string()))
    } else {
        Ok(())
    }
}

/// Run `task` for every item concurrently and return results in input order.
pub fn fan_out<I: Sync, T: Send>(items: &[I], task: impl Fn(&I) -> T + Sync) -> Vec<T> {
    thread::scope(|scope| {
        let handles: Vec<_> = items
            .iter()
            .map(|item| scope.spawn(|| task(item)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("fan-out task panicked"))
            .collect()
    })
}

/// `$XDG_RUNTIME_DIR/fleet/ssh`, else `$HOME/.cache/fleet/ssh`, mode 0700.
/// Without a usable directory, queries fall back to unshared connections.
fn prepare_control_dir(env: &ProcessEnv) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(|dir| PathBuf::from(dir).join("fleet"))
        .or_else(|| env.home.as_ref().map(|home| home.join(".cache/fleet")))?;
    let dir = base.join("ssh");
    fs::create_dir_all(&dir).ok()?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).ok()?;
    // macOS limits Unix socket paths to 104 bytes including the NUL. OpenSSH
    // binds `<dir>/<%C: 40 chars>.<16 random chars>` before renaming it.
    if dir.as_os_str().len() + 1 + 40 + 17 >= 104 {
        return None;
    }
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_round_trips_single_quotes() {
        assert_eq!(sh_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(wrap_for_login_shell("echo 'a'"), "sh -c 'echo '\\''a'\\'''");
    }

    #[test]
    fn rejects_backslashes_and_control_characters() {
        assert!(validate_script_value("/tmp/plain path").is_ok());
        assert!(validate_script_value("a\\b").is_err());
        assert!(validate_script_value("a\nb").is_err());
    }

    #[test]
    fn query_argv_reuses_connection_when_directory_is_available() {
        let dir = PathBuf::from("/tmp/fleet-ssh");
        let argv = query_ssh_argv("box", "true", Some(&dir), false);
        let argv: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv.first().map(String::as_str), Some("-n"));
        assert!(argv.contains(&"ControlMaster=auto".to_string()));
        assert!(argv.contains(&"ControlPath=/tmp/fleet-ssh/%C".to_string()));
        assert_eq!(&argv[argv.len() - 2..], ["box", "true"]);

        let unshared = query_ssh_argv("box", "true", None, true);
        assert!(!unshared.contains(&OsString::from("-n")));
        assert!(unshared.contains(&OsString::from("ControlMaster=no")));
    }

    #[test]
    fn local_timeout_kills_children_that_hold_stdout() {
        let runner = Runner {
            control_dir: None,
            timeout: Duration::from_millis(300),
        };
        let started = Instant::now();
        let output = runner.run(&Endpoint::Local, "sleep 30 & sleep 30");
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn fan_out_keeps_input_order() {
        let items = [3_u64, 1, 2];
        let results = fan_out(&items, |n| {
            thread::sleep(Duration::from_millis(n * 5));
            n * 10
        });
        assert_eq!(results, [30, 10, 20]);
    }
}
