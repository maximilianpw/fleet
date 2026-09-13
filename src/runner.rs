//! Foreground SSH child for a managed tunnel.
//!
//! The runner owns exactly one SSH process and a monotonic startup deadline.
//! launchd restarts failed tunnels; this process does not reconnect. Cleanup
//! signals only the owned child, never a name-based process sweep.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::launchd::{self, exit_status_code, send_signal};

pub const STARTUP_DEADLINE: Duration = Duration::from_secs(45);
pub const LISTENER_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
pub const LISTENER_PROBE_KILL_AFTER: Duration = Duration::from_secs(1);
pub const DEFAULT_POLL: Duration = Duration::from_millis(50);

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
    let mut child = match Command::new(&cfg.ssh_program)
        .args(&cfg.ssh_args)
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let _ = writeln!(
                io::stderr(),
                "fleet-tunnel-runner: failed to spawn ssh: {err}"
            );
            return 127;
        }
    };

    let poll = cfg.poll_interval.max(Duration::from_millis(1));
    let deadline = Instant::now() + cfg.startup_deadline;
    let mut healthy = false;

    loop {
        if let Some(status) = match child.try_wait() {
            Ok(status) => status,
            Err(err) => {
                let _ = writeln!(io::stderr(), "fleet-tunnel-runner: wait failed: {err}");
                kill_owned(&mut child);
                return 1;
            }
        } {
            return exit_status_code(status);
        }

        let sig = stop_signal(cfg);
        if sig != 0 {
            kill_owned(&mut child);
            return 128 + sig;
        }

        if !healthy && Instant::now() >= deadline {
            if let Some(status) = child.try_wait().ok().flatten() {
                return exit_status_code(status);
            }
            if child_owns_loopback_listen(cfg, child.id()) {
                healthy = true;
            } else {
                if let Some(status) = child.try_wait().ok().flatten() {
                    return exit_status_code(status);
                }
                kill_owned(&mut child);
                return 1;
            }
        }

        thread::sleep(poll);
    }
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

fn child_owns_loopback_listen(cfg: &RunnerConfig, pid: u32) -> bool {
    let mut cmd = Command::new(&cfg.lsof_program);
    cmd.args([
        "-nP",
        "-a",
        "-p",
        &pid.to_string(),
        &format!("-iTCP@127.0.0.1:{}", cfg.local_port),
        "-sTCP:LISTEN",
        "-t",
    ]);
    match launchd::run_with_deadline(
        &mut cmd,
        cfg.listener_probe,
        cfg.listener_kill_after,
        cfg.poll_interval,
    ) {
        Ok(output) => !output.timed_out && output.code == 0,
        Err(_) => false,
    }
}
