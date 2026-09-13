//! Runner child ownership, startup deadline, and signal cleanup tests.
//!
//! Fake `ssh`/`lsof` binaries and disposable children only. No live SSH.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fleet::runner::{self, RunnerConfig, STARTUP_DEADLINE};

static NEXT: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("fleet-runner-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn write_exec(path: &Path, body: &str) {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let tmp = path.with_extension("new");
    {
        let mut file = File::create(&tmp).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    let mut perms = fs::metadata(&tmp).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&tmp, perms).unwrap();
    fs::rename(&tmp, path).unwrap();
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a presence check.
    unsafe { kill(pid as i32, 0) == 0 }
}

struct Harness {
    dir: Scratch,
    ssh: PathBuf,
    lsof: PathBuf,
    args_file: PathBuf,
    pid_file: PathBuf,
    spawn_file: PathBuf,
    status_file: PathBuf,
    listen_file: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = scratch();
        let args_file = dir.0.join("args");
        let pid_file = dir.0.join("pid");
        let spawn_file = dir.0.join("spawns");
        let status_file = dir.0.join("status");
        let listen_file = dir.0.join("listen");
        fs::write(&spawn_file, "").unwrap();
        fs::write(&status_file, "ok\n").unwrap();
        fs::write(&listen_file, "no\n").unwrap();
        let ssh = dir.0.join("ssh");
        write_exec(
            &ssh,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$@" >'{args}'
printf '%s\n' "$$" >'{pid}'
printf x >>'{spawns}'
status=$(cat '{status}')
case "$status" in
  hang) exec sleep 30 ;;
  slow) sleep 0.2; exit 0 ;;
  near) sleep 1; exit 0 ;;
  exit) exit "$(cat '{code}')" ;;
  *) exit 0 ;;
esac
"#,
                args = args_file.display(),
                pid = pid_file.display(),
                spawns = spawn_file.display(),
                status = status_file.display(),
                code = dir.0.join("code").display(),
            ),
        );
        let lsof = dir.0.join("lsof");
        write_exec(
            &lsof,
            &format!(
                r#"#!/bin/sh
if [ "$(cat '{listen}')" = yes ]; then
  exit 0
fi
exit 1
"#,
                listen = listen_file.display(),
            ),
        );
        Self {
            dir,
            ssh,
            lsof,
            args_file,
            pid_file,
            spawn_file,
            status_file,
            listen_file,
        }
    }

    fn cfg(&self, status: &str, listen: &str) -> RunnerConfig {
        fs::write(&self.status_file, format!("{status}\n")).unwrap();
        fs::write(&self.listen_file, format!("{listen}\n")).unwrap();
        let mut cfg = RunnerConfig::production(
            3000,
            vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=10".into(),
                "-o".into(),
                "ExitOnForwardFailure=yes".into(),
                "-o".into(),
                "ForwardAgent=no".into(),
                "-o".into(),
                "ControlMaster=no".into(),
                "-o".into(),
                "ControlPath=none".into(),
                "-o".into(),
                "ServerAliveInterval=30".into(),
                "-o".into(),
                "ServerAliveCountMax=3".into(),
                "-N".into(),
                "-L".into(),
                "127.0.0.1:3000:localhost:3000".into(),
                "fleet-forward-workbox".into(),
            ],
        );
        cfg.ssh_program = self.ssh.clone().into();
        cfg.lsof_program = self.lsof.clone().into();
        // Most tests exercise child ownership rather than deadline expiry.
        // Keep their process scheduling budget generous enough for loaded
        // Darwin Nix builders; deadline-specific tests override this value.
        cfg.startup_deadline = Duration::from_secs(2);
        cfg.listener_probe = Duration::from_secs(2);
        cfg.listener_kill_after = Duration::from_millis(500);
        cfg.poll_interval = Duration::from_millis(5);
        cfg
    }

    fn child_pid(&self) -> Option<u32> {
        fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
    }

    fn spawn_count(&self) -> usize {
        fs::read_to_string(&self.spawn_file)
            .unwrap_or_default()
            .len()
    }

    fn wait_child_pid(&self) -> u32 {
        for _ in 0..500 {
            if let Some(pid) = self.child_pid() {
                return pid;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("ssh child did not record a pid");
    }
}

fn production_ssh_args() -> Vec<&'static str> {
    vec![
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=3",
        "-N",
        "-L",
        "127.0.0.1:3000:localhost:3000",
        "fleet-forward-workbox",
    ]
}

#[test]
fn production_startup_deadline_is_forty_five_seconds() {
    assert_eq!(STARTUP_DEADLINE, Duration::from_secs(45));
    let cfg = RunnerConfig::production(3000, Vec::new());
    assert_eq!(cfg.startup_deadline, Duration::from_secs(45));
}

#[test]
fn child_exit_before_deadline_is_preserved() {
    let harness = Harness::new();
    fs::write(harness.dir.0.join("code"), "7\n").unwrap();
    let cfg = harness.cfg("exit", "no");
    let code = runner::run(&cfg);
    assert_eq!(code, 7);
    assert_eq!(harness.spawn_count(), 1);
}

#[test]
fn hang_past_deadline_without_owned_listener_kills_only_the_child() {
    let harness = Harness::new();
    let mut cfg = harness.cfg("hang", "no");
    cfg.startup_deadline = Duration::from_millis(80);
    let start = Instant::now();
    let code = runner::run(&cfg);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(code, 1);
    let pid = harness.child_pid().expect("child pid");
    assert!(
        !pid_alive(pid),
        "owned ssh child leaked after startup failure"
    );
    assert_eq!(harness.spawn_count(), 1);
}

#[test]
fn healthy_listener_is_not_a_lifetime_cap() {
    let harness = Harness::new();
    let mut cfg = harness.cfg("slow", "yes");
    cfg.startup_deadline = Duration::from_millis(80);
    let start = Instant::now();
    let code = runner::run(&cfg);
    assert_eq!(code, 0);
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert_eq!(harness.spawn_count(), 1);
}

#[test]
fn wrong_process_owning_the_port_fails_startup() {
    let harness = Harness::new();
    let mut cfg = harness.cfg("hang", "no");
    cfg.startup_deadline = Duration::from_millis(80);
    let code = runner::run(&cfg);
    assert_eq!(code, 1);
}

#[test]
fn stop_during_startup_reaps_owned_child() {
    let harness = Harness::new();
    let mut cfg = harness.cfg("hang", "no");
    let stop = Arc::new(AtomicI32::new(0));
    cfg.stop = Arc::clone(&stop);
    let handle = thread::spawn(move || runner::run(&cfg));
    let pid = harness.wait_child_pid();
    stop.store(2, Ordering::SeqCst);
    let code = handle.join().unwrap();
    assert_eq!(code, 130);
    assert!(!pid_alive(pid));
}

#[test]
fn stop_after_healthy_listener_reaps_owned_child() {
    let harness = Harness::new();
    let mut cfg = harness.cfg("hang", "yes");
    cfg.startup_deadline = Duration::from_millis(80);
    let stop = Arc::new(AtomicI32::new(0));
    cfg.stop = Arc::clone(&stop);
    let handle = thread::spawn(move || runner::run(&cfg));
    let pid = harness.wait_child_pid();
    thread::sleep(Duration::from_millis(120));
    stop.store(15, Ordering::SeqCst);
    let code = handle.join().unwrap();
    assert_eq!(code, 143);
    assert!(!pid_alive(pid));
}

#[test]
fn argument_policy_is_passed_through_unchanged() {
    let harness = Harness::new();
    let cfg = harness.cfg("ok", "no");
    let code = runner::run(&cfg);
    assert_eq!(code, 0);
    let logged = fs::read_to_string(&harness.args_file).unwrap();
    let got: Vec<&str> = logged.lines().collect();
    assert_eq!(got, production_ssh_args());
}

#[test]
fn runner_does_not_reconnect_after_child_exit() {
    let harness = Harness::new();
    fs::write(harness.dir.0.join("code"), "1\n").unwrap();
    let cfg = harness.cfg("exit", "yes");
    let code = runner::run(&cfg);
    assert_eq!(code, 1);
    assert_eq!(harness.spawn_count(), 1);
}

#[test]
fn child_exit_near_deadline_does_not_target_a_reused_pid() {
    let harness = Harness::new();
    for _ in 0..3 {
        fs::write(&harness.spawn_file, "").unwrap();
        let _ = fs::remove_file(&harness.pid_file);
        fs::write(&harness.status_file, "near\n").unwrap();
        fs::write(&harness.listen_file, "no\n").unwrap();
        let mut cfg = harness.cfg("near", "no");
        cfg.startup_deadline = Duration::from_secs(1);
        let code = runner::run(&cfg);
        assert!(code == 0 || code == 1, "unexpected runner exit {code}");
        if let Some(pid) = harness.child_pid() {
            assert!(!pid_alive(pid), "runner left a child alive after returning");
        }
        assert_eq!(harness.spawn_count(), 1);
    }
}

#[test]
fn parse_args_requires_a_real_port() {
    assert!(runner::parse_args(["fleet-tunnel-runner".into()]).is_err());
    assert!(runner::parse_args(["fleet-tunnel-runner".into(), "0".into()]).is_err());
    let cfg =
        runner::parse_args(["fleet-tunnel-runner".into(), "3000".into(), "-N".into()]).unwrap();
    assert_eq!(cfg.local_port, 3000);
    assert_eq!(cfg.ssh_args, vec![std::ffi::OsString::from("-N")]);
}

#[test]
fn binary_int_during_startup_cleans_up_child() {
    let harness = Harness::new();
    fs::write(&harness.status_file, "hang\n").unwrap();
    fs::write(&harness.listen_file, "no\n").unwrap();
    let path = format!(
        "{}:{}",
        harness.dir.0.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_fleet-tunnel-runner"))
        .env("PATH", &path)
        .env("FLEET_TUNNEL_STARTUP_SECONDS", "1")
        .arg("3000")
        .args(production_ssh_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ssh_pid = harness.wait_child_pid();
    thread::sleep(Duration::from_millis(200));
    assert!(
        pid_alive(child.id()),
        "runner honored FLEET_TUNNEL_STARTUP_SECONDS and exited early"
    );
    unsafe {
        let _ = kill(child.id() as i32, 2);
    }
    let status = child.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let code = status.code().or_else(|| status.signal().map(|s| 128 + s));
        assert_eq!(code, Some(130));
    }
    assert!(!pid_alive(ssh_pid));
}

#[test]
fn binary_term_during_startup_cleans_up_child() {
    let harness = Harness::new();
    fs::write(&harness.status_file, "hang\n").unwrap();
    let path = format!(
        "{}:{}",
        harness.dir.0.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_fleet-tunnel-runner"))
        .env("PATH", &path)
        .arg("3000")
        .args(production_ssh_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ssh_pid = harness.wait_child_pid();
    unsafe {
        let _ = kill(child.id() as i32, 15);
    }
    let status = child.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let code = status.code().or_else(|| status.signal().map(|s| 128 + s));
        assert_eq!(code, Some(143));
    }
    assert!(!pid_alive(ssh_pid));
}
