//! Foreground SSH child supervision for a managed tunnel.
//!
//! The runner owns at most one SSH process, verifies its listener after a
//! monotonic startup deadline, and reconnects after failures. Cleanup signals
//! only the owned child, never a name-based process sweep.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::launchd::{exit_status_code, send_signal};

pub const STARTUP_DEADLINE: Duration = Duration::from_secs(45);
pub const LISTENER_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
pub const LISTENER_PROBE_KILL_AFTER: Duration = Duration::from_secs(1);
pub const DEFAULT_POLL: Duration = Duration::from_millis(50);
pub const RETRY_BACKOFF: Duration = Duration::from_secs(30);

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}

static STOP: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_int(_: i32) {
    STOP.store(SIGINT, Ordering::SeqCst);
}

extern "C" fn on_term(_: i32) {
    STOP.store(SIGTERM, Ordering::SeqCst);
}

pub struct RunnerConfig {
    pub local_port: u16,
    pub ssh_args: Vec<OsString>,
    pub ssh_program: OsString,
    pub lsof_program: OsString,
    pub startup_deadline: Duration,
    pub listener_probe: Duration,
    pub listener_kill_after: Duration,
    pub poll_interval: Duration,
    pub retry_backoff: Duration,
    pub stop: Arc<AtomicI32>,
}

impl RunnerConfig {
    pub fn production(local_port: u16, ssh_args: Vec<OsString>) -> Self {
        Self {
            local_port,
            ssh_args,
            ssh_program: OsString::from("ssh"),
            lsof_program: OsString::from("lsof"),
            startup_deadline: STARTUP_DEADLINE,
            listener_probe: LISTENER_PROBE_TIMEOUT,
            listener_kill_after: LISTENER_PROBE_KILL_AFTER,
            poll_interval: DEFAULT_POLL,
            retry_backoff: RETRY_BACKOFF,
            stop: Arc::new(AtomicI32::new(0)),
        }
    }
}

/// Parse `fleet-tunnel-runner LOCAL_PORT SSH_ARGS...`. Does not install
/// signal handlers; the binary calls [`install_stop_handlers`] first.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<RunnerConfig, i32> {
    let mut args = args.into_iter();
    let _argv0 = args.next();
    let Some(port_arg) = args.next() else {
        let _ = writeln!(io::stderr(), "fleet-tunnel-runner: local port required");
        return Err(2);
    };
    let Some(port) = parse_port(&port_arg) else {
        let _ = writeln!(
            io::stderr(),
            "fleet-tunnel-runner: invalid local port: {}",
            port_arg.to_string_lossy()
        );
        return Err(2);
    };
    Ok(RunnerConfig::production(port, args.collect()))
}

fn parse_port(arg: &OsString) -> Option<u16> {
    let text = arg.to_str()?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u32 = text.parse().ok()?;
    if (1..=65535).contains(&port) {
        u16::try_from(port).ok()
    } else {
        None
    }
}

/// Process-wide INT/TERM handlers. They only store a signal number; [`run`]
/// reaps the owned child. Do not call this from in-process tests.
pub fn install_stop_handlers() {
    STOP.store(0, Ordering::SeqCst);
    // SAFETY: handlers only store to an atomic.
    unsafe {
        let _ = signal(SIGINT, on_int);
        let _ = signal(SIGTERM, on_term);
    }
}

pub fn run(cfg: &RunnerConfig) -> i32 {
    loop {
        if let Some(code) = stop_exit_code(cfg) {
            return code;
        }

        let mut child = match Command::new(&cfg.ssh_program)
            .args(&cfg.ssh_args)
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                let _ = writeln!(
                    io::stderr(),
                    "fleet-tunnel-runner: failed to spawn ssh: {err}; retrying"
                );
                if let Some(code) = wait_for_retry_or_stop(cfg) {
                    return code;
                }
                continue;
            }
        };

        match supervise_child(cfg, &mut child) {
            ChildOutcome::Stop(code) => return code,
            ChildOutcome::Retry => {
                if let Some(code) = wait_for_retry_or_stop(cfg) {
                    return code;
                }
            }
        }
    }
}

enum ChildOutcome {
    Retry,
    Stop(i32),
}

fn supervise_child(cfg: &RunnerConfig, child: &mut std::process::Child) -> ChildOutcome {
    let poll = cfg.poll_interval.max(Duration::from_millis(1));
    let deadline = Instant::now() + cfg.startup_deadline;
    let mut healthy = false;

    loop {
        if let Some(code) = stop_exit_code(cfg) {
            kill_owned(child);
            return ChildOutcome::Stop(code);
        }

        if let Some(status) = match child.try_wait() {
            Ok(status) => status,
            Err(err) => {
                let _ = writeln!(io::stderr(), "fleet-tunnel-runner: wait failed: {err}");
                kill_owned(child);
                return stop_exit_code(cfg)
                    .map(ChildOutcome::Stop)
                    .unwrap_or(ChildOutcome::Retry);
            }
        } {
            if let Some(code) = stop_exit_code(cfg) {
                return ChildOutcome::Stop(code);
            }
            let code = exit_status_code(status);
            let _ = writeln!(
                io::stderr(),
                "fleet-tunnel-runner: ssh exited with status {code}; retrying"
            );
            return ChildOutcome::Retry;
        }

        if !healthy && Instant::now() >= deadline {
            if let Some(status) = child.try_wait().ok().flatten() {
                if let Some(code) = stop_exit_code(cfg) {
                    return ChildOutcome::Stop(code);
                }
                let code = exit_status_code(status);
                let _ = writeln!(
                    io::stderr(),
                    "fleet-tunnel-runner: ssh exited with status {code}; retrying"
                );
                return ChildOutcome::Retry;
            }
            match child_owns_loopback_listen(cfg, child.id()) {
                ListenerProbe::Listening => healthy = true,
                ListenerProbe::Stopped(code) => {
                    kill_owned(child);
                    return ChildOutcome::Stop(code);
                }
                ListenerProbe::NotListening => {
                    if let Some(code) = stop_exit_code(cfg) {
                        kill_owned(child);
                        return ChildOutcome::Stop(code);
                    }
                    if child.try_wait().ok().flatten().is_none() {
                        kill_owned(child);
                    }
                    let _ = writeln!(
                        io::stderr(),
                        "fleet-tunnel-runner: ssh did not own the expected listener; retrying"
                    );
                    return stop_exit_code(cfg)
                        .map(ChildOutcome::Stop)
                        .unwrap_or(ChildOutcome::Retry);
                }
            }
        }

        thread::sleep(poll);
    }
}

fn wait_for_retry_or_stop(cfg: &RunnerConfig) -> Option<i32> {
    let poll = cfg.poll_interval.max(Duration::from_millis(1));
    let deadline = Instant::now() + cfg.retry_backoff.max(poll);
    loop {
        if let Some(code) = stop_exit_code(cfg) {
            return Some(code);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        thread::sleep(poll.min(remaining));
    }
}

fn stop_exit_code(cfg: &RunnerConfig) -> Option<i32> {
    let sig = stop_signal(cfg);
    (sig != 0).then_some(128 + sig)
}

fn stop_signal(cfg: &RunnerConfig) -> i32 {
    let from_cfg = cfg.stop.load(Ordering::SeqCst);
    if from_cfg != 0 {
        from_cfg
    } else {
        STOP.load(Ordering::SeqCst)
    }
}

fn kill_owned(child: &mut std::process::Child) {
    send_signal(child.id(), SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

enum ListenerProbe {
    Listening,
    NotListening,
    Stopped(i32),
}

fn child_owns_loopback_listen(cfg: &RunnerConfig, pid: u32) -> ListenerProbe {
    let mut cmd = Command::new(&cfg.lsof_program);
    cmd.args([
        "-nP",
        "-a",
        "-p",
        &pid.to_string(),
        &format!("-iTCP@127.0.0.1:{}", cfg.local_port),
        "-sTCP:LISTEN",
        "-t",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut probe = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return ListenerProbe::NotListening,
    };
    let poll = cfg.poll_interval.max(Duration::from_millis(1));
    let deadline = Instant::now() + cfg.listener_probe;
    loop {
        if let Some(code) = stop_exit_code(cfg) {
            kill_owned(&mut probe);
            return ListenerProbe::Stopped(code);
        }
        match probe.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    ListenerProbe::Listening
                } else {
                    ListenerProbe::NotListening
                };
            }
            Ok(None) => {}
            Err(_) => {
                kill_owned(&mut probe);
                return ListenerProbe::NotListening;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(poll.min(remaining));
    }

    send_signal(probe.id(), SIGTERM);
    let kill_at = Instant::now() + cfg.listener_kill_after;
    loop {
        if let Some(code) = stop_exit_code(cfg) {
            kill_owned(&mut probe);
            return ListenerProbe::Stopped(code);
        }
        match probe.try_wait() {
            Ok(Some(_)) => return ListenerProbe::NotListening,
            Ok(None) => {}
            Err(_) => {
                kill_owned(&mut probe);
                return ListenerProbe::NotListening;
            }
        }
        let remaining = kill_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            kill_owned(&mut probe);
            return ListenerProbe::NotListening;
        }
        thread::sleep(poll.min(remaining));
    }
}
