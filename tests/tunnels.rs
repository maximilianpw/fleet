//! Supervisor, doctor, listener ownership, and managed-delete tests.
//!
//! Fakes replace launchctl and SSH. Nothing here calls a live daemon or host.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fleet::doctor::{
    self, classify_ssh_stderr, remote_listen_command, DoctorDeadlines, DoctorError, DoctorHost,
    SshOutcome, SshProbe, SshRequest, SshState, SystemSsh,
};
use fleet::launchd::{
    self, parse_launchctl_print, parse_print_disabled, tunnel_label, Launchctl, LaunchctlOutput,
    SystemLaunchctl, FLEET_TUNNEL_LABEL_PREFIX,
};
use fleet::tunnels::{
    self, classify_local_listen, managed_delete_verdict, ListenError, ListenProbe,
    ListenerOwnership, ManagedDeleteVerdict, Mapping, ProcessIdentity, Supervisor, TunnelContext,
    TunnelError, LAUNCHCTL_REQUIRED_MESSAGE, MACOS_ONLY_MESSAGE, NO_SUPERVISOR_MESSAGE,
    NO_TUNNELS_CONFIGURED,
};

const UID: u32 = 501;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let n = NEXT_SCRATCH.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("fleet-tunnels-{}-{n}", std::process::id()));
    fs::create_dir_all(dir.join("Library/LaunchAgents")).unwrap();
    Scratch(dir)
}

fn fixture(rel: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel),
    )
    .unwrap_or_else(|err| panic!("fixture {rel}: {err}"))
}

fn mappings() -> Vec<Mapping> {
    vec![
        Mapping {
            local_port: 3000,
            host: "workbox".into(),
            remote_port: 3000,
            remote_host: "localhost".into(),
            label: tunnel_label(3000),
        },
        Mapping {
            local_port: 5173,
            host: "workbox".into(),
            remote_port: 5173,
            remote_host: "localhost".into(),
            label: tunnel_label(5173),
        },
    ]
}

fn ctx<'a>(home: &'a Path, maps: &'a [Mapping], supervisor: Supervisor) -> TunnelContext<'a> {
    TunnelContext {
        supervisor,
        mappings: maps,
        home,
        uid: UID,
    }
}

fn doctor_host() -> DoctorHost {
    DoctorHost {
        canonical: "workbox".into(),
        ssh_target: "workbox".into(),
        is_local: false,
    }
}

fn fast_deadlines() -> DoctorDeadlines {
    DoctorDeadlines {
        ssh: Duration::from_millis(200),
        ssh_kill_after: Duration::from_millis(50),
        probe: Duration::from_millis(200),
        probe_kill_after: Duration::from_millis(50),
        poll: Duration::from_millis(5),
    }
}

struct FakeLaunchd {
    commands: RefCell<Vec<String>>,
    disabled: RefCell<HashSet<String>>,
    loaded: RefCell<HashSet<String>>,
    running: RefCell<HashMap<String, u32>>,
    fail: RefCell<HashSet<String>>,
    print_unknown: RefCell<bool>,
    print_disabled_fail: RefCell<bool>,
    available: bool,
    disabled_value: String,
}

impl FakeLaunchd {
    fn new() -> Self {
        Self {
            commands: RefCell::new(Vec::new()),
            disabled: RefCell::new(HashSet::new()),
            loaded: RefCell::new(HashSet::new()),
            running: RefCell::new(HashMap::new()),
            fail: RefCell::new(HashSet::new()),
            print_unknown: RefCell::new(false),
            print_disabled_fail: RefCell::new(false),
            available: true,
            disabled_value: "true".into(),
        }
    }

    fn start_job(&self, port: u16, pid: u32) {
        let label = tunnel_label(port);
        self.loaded.borrow_mut().insert(label.clone());
        self.running.borrow_mut().insert(label, pid);
    }

    fn recorded(&self) -> Vec<String> {
        self.commands.borrow().clone()
    }

    fn print_count(&self, port: u16) -> usize {
        let needle = format!("print gui/{UID}/{}", tunnel_label(port));
        self.recorded()
            .iter()
            .filter(|line| *line == &needle)
            .count()
    }

    fn has_verb(&self, verb: &str) -> bool {
        self.recorded()
            .iter()
            .any(|line| line.split_whitespace().next() == Some(verb))
    }

    fn fail_verb(&self, verb: &str) {
        self.fail.borrow_mut().insert(verb.to_string());
    }
}

impl Launchctl for FakeLaunchd {
    fn run(&self, args: &[OsString]) -> LaunchctlOutput {
        let rendered: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        self.commands.borrow_mut().push(rendered.join(" "));
        let verb = rendered.first().map(String::as_str).unwrap_or("");
        if self.fail.borrow().contains(verb) {
            return LaunchctlOutput::Ran {
                code: 1,
                stdout: String::new(),
                stderr: format!("{verb} failed"),
            };
        }
        match verb {
            "print-disabled" => {
                if *self.print_disabled_fail.borrow() {
                    return LaunchctlOutput::Ran {
                        code: 1,
                        stdout: String::new(),
                        stderr: "print-disabled failed".into(),
                    };
                }
                let mut body = String::from("disabled services = {\n");
                for label in self.disabled.borrow().iter() {
                    body.push_str(&format!("\t\"{label}\" => {}\n", self.disabled_value));
                }
                body.push_str("}\n");
                LaunchctlOutput::Ran {
                    code: 0,
                    stdout: body,
                    stderr: String::new(),
                }
            }
            "print" => {
                if *self.print_unknown.borrow() {
                    return LaunchctlOutput::Spawn(io::Error::other("launchctl crashed"));
                }
                let target = rendered.get(1).cloned().unwrap_or_default();
                let label = target.rsplit('/').next().unwrap_or("").to_string();
                let loaded = self.loaded.borrow().contains(&label);
                let running = self.running.borrow().get(&label).copied();
                if !loaded && running.is_none() {
                    return LaunchctlOutput::Ran {
                        code: 1,
                        stdout: String::new(),
                        stderr: format!("Could not find service {target}"),
                    };
                }
                if let Some(pid) = running {
                    LaunchctlOutput::Ran {
                        code: 0,
                        stdout: format!("state = running\npid = {pid}\n"),
                        stderr: String::new(),
                    }
                } else {
                    LaunchctlOutput::Ran {
                        code: 0,
                        stdout: "state = not running\n".into(),
                        stderr: String::new(),
                    }
                }
            }
            "disable" => {
                let label = label_from_target(rendered.get(1).map(String::as_str).unwrap_or(""));
                self.disabled.borrow_mut().insert(label);
                ok()
            }
            "enable" => {
                let label = label_from_target(rendered.get(1).map(String::as_str).unwrap_or(""));
                self.disabled.borrow_mut().remove(&label);
                ok()
            }
            "bootout" => {
                let target = if rendered.get(1).map(String::as_str) == Some("--wait") {
                    rendered.get(2).cloned().unwrap_or_default()
                } else {
                    rendered.get(1).cloned().unwrap_or_default()
                };
                let label = label_from_target(&target);
                self.running.borrow_mut().remove(&label);
                self.loaded.borrow_mut().remove(&label);
                ok()
            }
            "bootstrap" => {
                let plist = rendered.get(2).cloned().unwrap_or_default();
                let label = Path::new(&plist)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                self.loaded.borrow_mut().insert(label.clone());
                if self.disabled.borrow().contains(&label) {
                    self.running.borrow_mut().remove(&label);
                } else {
                    self.running.borrow_mut().insert(label, 42);
                }
                ok()
            }
            "kickstart" => {
                let label = label_from_target(rendered.get(1).map(String::as_str).unwrap_or(""));
                if self.disabled.borrow().contains(&label) {
                    return LaunchctlOutput::Ran {
                        code: 1,
                        stdout: String::new(),
                        stderr: "kickstart failed: service disabled".into(),
                    };
                }
                self.loaded.borrow_mut().insert(label.clone());
                self.running.borrow_mut().insert(label, 42);
                ok()
            }
            _ => LaunchctlOutput::Ran {
                code: 90,
                stdout: String::new(),
                stderr: format!("unknown command {verb}"),
            },
        }
    }

    fn is_available(&self) -> bool {
        self.available
    }
}

fn label_from_target(target: &str) -> String {
    target.rsplit('/').next().unwrap_or(target).to_string()
}

fn ok() -> LaunchctlOutput {
    LaunchctlOutput::Ran {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
}

#[derive(Default)]
struct FakeListen {
    pids: RefCell<HashMap<u16, Vec<u32>>>,
    tcp: RefCell<HashSet<u16>>,
    identities: RefCell<HashMap<u32, ProcessIdentity>>,
    unavailable: RefCell<HashSet<u16>>,
}

impl FakeListen {
    fn own(&self, port: u16, job_pid: u32) {
        self.pids.borrow_mut().insert(port, vec![job_pid]);
        self.tcp.borrow_mut().insert(port);
        self.identities.borrow_mut().insert(
            job_pid,
            ProcessIdentity {
                ppid: Some(1),
                comm: "fleet-tunnel-runner".into(),
            },
        );
    }

    fn ssh_child(&self, port: u16, job_pid: u32, child: u32) {
        self.pids.borrow_mut().insert(port, vec![child]);
        self.tcp.borrow_mut().insert(port);
        self.identities.borrow_mut().insert(
            child,
            ProcessIdentity {
                ppid: Some(job_pid),
                comm: "/nix/store/fixture/bin/ssh".into(),
            },
        );
    }
}

impl ListenProbe for FakeListen {
    fn listening_pids(&self, port: u16) -> Result<Vec<u32>, ListenError> {
        if self.unavailable.borrow().contains(&port) {
            return Err(ListenError {
                message: "lsof unavailable".into(),
            });
        }
        Ok(self.pids.borrow().get(&port).cloned().unwrap_or_default())
    }

    fn loopback_connects(&self, port: u16) -> bool {
        self.tcp.borrow().contains(&port)
    }

    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        self.identities.borrow().get(&pid).cloned()
    }
}

#[derive(Clone, Copy)]
enum FakeSshStatus {
    Ok,
    Auth,
    Unavailable,
}

#[derive(Clone, Copy)]
enum FakeRemote {
    Up,
    Down,
    Failed,
}

struct FakeSsh {
    status: FakeSshStatus,
    remote: FakeRemote,
    calls: RefCell<Vec<SshRequest>>,
}

impl FakeSsh {
    fn ok() -> Self {
        Self {
            status: FakeSshStatus::Ok,
            remote: FakeRemote::Up,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn probed_tcp(&self) -> bool {
        self.calls.borrow().iter().any(|call| {
            call.remote_command
                .as_deref()
                .is_some_and(|c| c.contains("/dev/tcp/"))
        })
    }
}

impl SshProbe for FakeSsh {
    fn run(&self, request: &SshRequest, _deadlines: DoctorDeadlines) -> SshOutcome {
        self.calls.borrow_mut().push(request.clone());
        if request.remote_command.is_some() {
            return match self.remote {
                FakeRemote::Up => SshOutcome {
                    code: 0,
                    stderr: String::new(),
                },
                FakeRemote::Down => SshOutcome {
                    code: 1,
                    stderr: String::new(),
                },
                FakeRemote::Failed => SshOutcome {
                    code: 255,
                    stderr: "ssh: Connection reset".into(),
                },
            };
        }
        match self.status {
            FakeSshStatus::Ok => SshOutcome {
                code: 0,
                stderr: String::new(),
            },
            FakeSshStatus::Auth => SshOutcome {
                code: 255,
                stderr: fixture("doctor/ssh-auth.stderr"),
            },
            FakeSshStatus::Unavailable => SshOutcome {
                code: 255,
                stderr: fixture("doctor/ssh-unavailable.stderr"),
            },
        }
    }
}

fn write_plist(home: &Path, port: u16) {
    let path = launchd::launch_agent_plist(home, &tunnel_label(port));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "placeholder\n").unwrap();
}

fn status_text(ctx: &TunnelContext<'_>, launchd: &FakeLaunchd, listen: &FakeListen) -> String {
    let mut out = Vec::new();
    tunnels::print_status(&mut out, ctx, launchd, listen).unwrap();
    String::from_utf8(out).unwrap()
}

fn doctor_text(
    host: &DoctorHost,
    ctx: &TunnelContext<'_>,
    launchd: &FakeLaunchd,
    listen: &FakeListen,
    ssh: &FakeSsh,
) -> (Result<(), DoctorError>, String) {
    let mut out = Vec::new();
    let result = doctor::doctor(&mut out, host, ctx, launchd, listen, ssh, fast_deadlines());
    (result, String::from_utf8(out).unwrap())
}

#[test]
fn parse_running_fixture_takes_state_and_pid_from_one_body() {
    let fields = parse_launchctl_print(&fixture("launchd/print-running.txt"));
    assert!(fields.running);
    assert_eq!(fields.pid, Some(4242));
}

#[test]
fn parse_not_running_does_not_treat_not_as_running() {
    let fields = parse_launchctl_print(&fixture("launchd/print-not-running.txt"));
    assert!(!fields.running);
    assert_eq!(fields.pid, None);
}

#[test]
fn disabled_label_is_exact_so_port_30000_does_not_pause_3000() {
    let labels = parse_print_disabled(&fixture("launchd/print-disabled.txt"));
    assert!(labels.contains(&tunnel_label(30000)));
    assert!(!labels.contains(&tunnel_label(3000)));
    assert!(FLEET_TUNNEL_LABEL_PREFIX.starts_with("org.nix-community.home."));
}

#[test]
fn disabled_alternate_value_pauses_the_quoted_label() {
    let labels = parse_print_disabled(&fixture("launchd/print-disabled-alt.txt"));
    assert!(labels.contains(&tunnel_label(3000)));
}

#[test]
fn status_with_no_mappings() {
    let home = scratch();
    let maps: Vec<Mapping> = Vec::new();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let text = status_text(&ctx, &FakeLaunchd::new(), &FakeListen::default());
    assert!(text.contains("LOCAL"));
    assert!(text.contains(NO_TUNNELS_CONFIGURED));
}

#[test]
fn status_without_supervisor_explains_macos_jobs() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::None);
    let launchd = FakeLaunchd::new();
    let text = status_text(&ctx, &launchd, &FakeListen::default());
    assert!(text.contains("unsupervised"));
    assert!(text.contains("none"));
    assert!(text.contains(MACOS_ONLY_MESSAGE));
    assert!(launchd.recorded().is_empty());
}

#[test]
fn status_uses_one_job_read_per_mapping() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(3000, 42);
    listen.own(5173, 42);
    let text = status_text(&ctx, &launchd, &listen);
    assert_eq!(launchd.print_count(3000), 1);
    assert_eq!(launchd.print_count(5173), 1);
    assert_eq!(
        launchd
            .recorded()
            .iter()
            .filter(|line| line.starts_with("print-disabled "))
            .count(),
        2
    );
    assert!(text.contains("running"));
    assert!(text.contains("owned"));
    assert!(text.contains("3000"));
    assert!(text.contains("5173"));
}

#[test]
fn resume_already_running_does_not_mutate() {
    let home = scratch();
    write_plist(&home.0, 3000);
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    listen.own(3000, 42);
    launchd.commands.borrow_mut().clear();
    let msg = tunnels::resume(3000, &ctx, &launchd, &listen).unwrap();
    assert!(msg.contains("already running"));
    assert_eq!(launchd.print_count(3000), 1);
    assert!(!launchd.has_verb("enable"));
    assert!(!launchd.has_verb("bootstrap"));
    assert!(!launchd.has_verb("kickstart"));
}

#[test]
fn pause_only_requested_job_and_survives_relogin() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(3000, 42);
    listen.own(5173, 42);
    let msg = tunnels::pause(3000, &ctx, &launchd).unwrap();
    assert!(msg.contains("paused managed tunnel for local port 3000"));
    let recorded = launchd.recorded().join("\n");
    assert!(recorded.contains(&format!("disable gui/{UID}/{}", tunnel_label(3000))));
    assert!(recorded.contains(&format!("bootout --wait gui/{UID}/{}", tunnel_label(3000))));
    assert!(!recorded.contains(&tunnel_label(5173)));
    assert!(launchd.disabled.borrow().contains(&tunnel_label(3000)));
    assert!(!launchd.running.borrow().contains_key(&tunnel_label(3000)));
    assert!(launchd.running.borrow().contains_key(&tunnel_label(5173)));

    launchd.running.borrow_mut().clear();
    launchd.loaded.borrow_mut().clear();
    listen.pids.borrow_mut().clear();
    listen.tcp.borrow_mut().clear();
    let paused = status_text(&ctx, &launchd, &listen);
    assert!(paused.contains("paused"));
    assert!(paused.contains("3000"));
    assert!(!paused
        .lines()
        .any(|line| line.contains("3000") && line.contains("running")));
}

#[test]
fn bootstrap_of_disabled_job_does_not_start_it() {
    let home = scratch();
    write_plist(&home.0, 3000);
    let launchd = FakeLaunchd::new();
    launchd.disabled.borrow_mut().insert(tunnel_label(3000));
    let out = launchd::launchctl_bootstrap(
        &launchd,
        &launchd::gui_domain(UID),
        &launchd::launch_agent_plist(&home.0, &tunnel_label(3000)),
    );
    assert!(out.success());
    assert!(launchd.loaded.borrow().contains(&tunnel_label(3000)));
    assert!(!launchd.running.borrow().contains_key(&tunnel_label(3000)));
}

#[test]
fn resume_occupied_refuses_and_keeps_pause() {
    let home = scratch();
    write_plist(&home.0, 3000);
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.disabled.borrow_mut().insert(tunnel_label(3000));
    listen.tcp.borrow_mut().insert(3000);
    launchd.commands.borrow_mut().clear();
    let err = tunnels::resume(3000, &ctx, &launchd, &listen).unwrap_err();
    assert!(matches!(err, TunnelError::PortOccupied { port: 3000 }));
    assert!(err.to_string().contains("unrelated process"));
    assert!(err.to_string().contains("not killing"));
    assert_eq!(err.exit_code(), 1);
    assert!(launchd.disabled.borrow().contains(&tunnel_label(3000)));
    assert!(!launchd.has_verb("enable"));
    assert!(!launchd.has_verb("kickstart"));
}

#[test]
fn resume_missing_job_bootstraps_and_refreshes_before_kickstart() {
    let home = scratch();
    write_plist(&home.0, 3000);
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    tunnels::resume(3000, &ctx, &launchd, &listen).unwrap();
    let recorded = launchd.recorded();
    assert!(recorded.iter().any(|line| line.starts_with("bootstrap ")));
    assert_eq!(launchd.print_count(3000), 3);
    assert!(!launchd.has_verb("kickstart"));
}

#[test]
fn resume_loaded_stopped_uses_kickstart() {
    let home = scratch();
    write_plist(&home.0, 3000);
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.loaded.borrow_mut().insert(tunnel_label(3000));
    tunnels::resume(3000, &ctx, &launchd, &listen).unwrap();
    assert!(launchd.has_verb("kickstart"));
    assert!(!launchd.has_verb("bootstrap"));
    assert!(launchd.running.borrow().contains_key(&tunnel_label(3000)));
}

#[test]
fn pause_and_resume_report_mutation_failures() {
    let home = scratch();
    write_plist(&home.0, 3000);
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let listen = FakeListen::default();

    let launchd = FakeLaunchd::new();
    launchd.start_job(3000, 42);
    launchd.fail_verb("disable");
    let err = tunnels::pause(3000, &ctx, &launchd).unwrap_err();
    assert!(matches!(err, TunnelError::DisableFailed { .. }));
    assert_eq!(err.exit_code(), 1);

    let launchd = FakeLaunchd::new();
    launchd.start_job(3000, 42);
    launchd.fail_verb("bootout");
    let err = tunnels::pause(3000, &ctx, &launchd).unwrap_err();
    assert!(matches!(err, TunnelError::BootoutFailed { .. }));
    assert!(launchd.disabled.borrow().contains(&tunnel_label(3000)));

    let launchd = FakeLaunchd::new();
    launchd.fail_verb("enable");
    let err = tunnels::resume(3000, &ctx, &launchd, &listen).unwrap_err();
    assert!(matches!(err, TunnelError::EnableFailed { .. }));

    let launchd = FakeLaunchd::new();
    launchd.fail_verb("bootstrap");
    let err = tunnels::resume(3000, &ctx, &launchd, &listen).unwrap_err();
    assert!(matches!(err, TunnelError::BootstrapFailed { .. }));
    assert!(!launchd.disabled.borrow().contains(&tunnel_label(3000)));

    let launchd = FakeLaunchd::new();
    launchd.loaded.borrow_mut().insert(tunnel_label(3000));
    launchd.fail_verb("kickstart");
    let err = tunnels::resume(3000, &ctx, &launchd, &listen).unwrap_err();
    assert!(matches!(err, TunnelError::KickstartFailed { .. }));
    assert!(err
        .to_string()
        .contains("failed to start managed tunnel job"));
}

#[test]
fn pause_requires_supervisor_and_known_port() {
    let home = scratch();
    let maps = mappings();
    let none = ctx(&home.0, &maps, Supervisor::None);
    let err = tunnels::pause(3000, &none, &FakeLaunchd::new()).unwrap_err();
    assert!(matches!(err, TunnelError::NoSupervisor));
    assert_eq!(err.exit_code(), 2);
    assert_eq!(err.to_string(), NO_SUPERVISOR_MESSAGE);

    let mut launchd = FakeLaunchd::new();
    launchd.available = false;
    let darwin = ctx(&home.0, &maps, Supervisor::Launchd);
    let err = tunnels::pause(3000, &darwin, &launchd).unwrap_err();
    assert!(matches!(err, TunnelError::LaunchctlRequired));
    assert_eq!(err.to_string(), LAUNCHCTL_REQUIRED_MESSAGE);

    let err = tunnels::pause(9, &darwin, &FakeLaunchd::new()).unwrap_err();
    assert!(matches!(err, TunnelError::NoMapping { port: 9 }));
}

#[test]
fn doctor_healthy_does_not_claim_end_to_end() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(3000, 42);
    listen.own(5173, 42);
    let ssh = FakeSsh::ok();
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &ssh);
    result.unwrap();
    assert_eq!(launchd.print_count(3000), 1);
    assert_eq!(launchd.print_count(5173), 1);
    assert!(text.contains("SSH: reachable"));
    assert!(text.contains("tunnel 3000: supervisor=running local=owned remote=listening"));
    assert!(text.contains("tunnel 5173: supervisor=running local=owned remote=listening"));
    assert!(!text.to_lowercase().contains("end-to-end healthy"));
    assert!(!text.to_lowercase().contains("everything is healthy"));
    let args = doctor::ssh_argv("workbox", None);
    let joined: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for needle in [
        "BatchMode=yes",
        "ConnectTimeout=10",
        "ControlMaster=no",
        "ForwardAgent=no",
        "-n",
    ] {
        assert!(joined.iter().any(|a| a == needle), "missing {needle}");
    }
}

#[test]
fn doctor_remote_command_parses_as_fish() {
    let command = remote_listen_command("localhost", 5173);
    assert_eq!(
        command,
        "bash -c 'command -v timeout >/dev/null || exit 69; if timeout 3 bash -c \"exec 3<>/dev/tcp/localhost/5173\"; then exit 0; else exit 1; fi'"
    );
    match Command::new("fish")
        .args(["--no-config", "--no-execute", "-c", &command])
        .status()
    {
        Ok(status) => assert!(status.success(), "fish rejected doctor remote command"),
        Err(error) if std::env::var_os("FLEET_REQUIRE_FISH").is_some() => {
            panic!("fish is required for this test: {error}")
        }
        Err(_) => eprintln!("skipping fish doctor-command parse test: fish is unavailable"),
    }
}

#[test]
fn doctor_alias_uses_canonical_mappings() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(3000, 42);
    listen.own(5173, 42);
    let host = DoctorHost {
        canonical: "workbox".into(),
        ssh_target: "dev".into(),
        is_local: false,
    };
    let (result, text) = doctor_text(&host, &ctx, &launchd, &listen, &FakeSsh::ok());
    result.unwrap();
    assert!(text.contains("tunnel 3000:"));
    assert!(text.contains("tunnel 5173:"));
}

#[test]
fn listener_ownership_accepts_job_or_direct_ssh_child() {
    let listen = FakeListen::default();
    listen.own(3000, 42);
    assert_eq!(
        classify_local_listen(3000, Some(42), &listen),
        ListenerOwnership::Owned
    );
    listen.ssh_child(3000, 42, 43);
    assert_eq!(
        classify_local_listen(3000, Some(42), &listen),
        ListenerOwnership::Owned
    );
    listen.pids.borrow_mut().insert(3000, vec![44]);
    listen.identities.borrow_mut().insert(
        44,
        ProcessIdentity {
            ppid: Some(43),
            comm: "/nix/store/fixture/bin/ssh".into(),
        },
    );
    assert_eq!(
        classify_local_listen(3000, Some(42), &listen),
        ListenerOwnership::Unrelated
    );
    listen.pids.borrow_mut().insert(3000, vec![99]);
    listen.identities.borrow_mut().remove(&99);
    assert_eq!(
        classify_local_listen(3000, Some(42), &listen),
        ListenerOwnership::Unrelated
    );
}

#[test]
fn empty_lsof_with_tcp_success_is_unrelated_ipv4_ipv6_conflict() {
    let listen = FakeListen::default();
    listen.tcp.borrow_mut().insert(3000);
    assert_eq!(
        classify_local_listen(3000, Some(42), &listen),
        ListenerOwnership::Unrelated
    );
}

#[test]
fn unavailable_lsof_is_unknown_not_none() {
    let listen = FakeListen::default();
    listen.unavailable.borrow_mut().insert(3000);
    assert_eq!(
        classify_local_listen(3000, Some(42), &listen),
        ListenerOwnership::Unknown
    );
}

#[test]
fn running_without_owned_listener_is_unhealthy() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(5173, 42);
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &FakeSsh::ok());
    assert!(result.is_err());
    assert!(text.contains("supervisor=running local=none"));
    assert!(text
        .contains("unhealthy: managed tunnel for local port 3000 is not listening on 127.0.0.1"));
}

#[test]
fn doctor_remote_down_is_not_local_health() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(3000, 42);
    listen.own(5173, 42);
    let ssh = FakeSsh {
        remote: FakeRemote::Down,
        ..FakeSsh::ok()
    };
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &ssh);
    assert!(matches!(result, Err(DoctorError::Unhealthy)));
    assert!(text.contains("SSH: reachable"));
    assert!(text.contains("remote=down"));
    assert!(text.contains("unhealthy: remote app is not listening on workbox localhost:3000"));
    assert!(text.contains("local=owned"));
}

#[test]
fn doctor_unrelated_local_occupant() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    listen.tcp.borrow_mut().insert(3000);
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &FakeSsh::ok());
    assert!(result.is_err());
    assert!(text.contains("local=unrelated"));
    assert!(text.contains("unhealthy: local port 3000 is occupied by an unrelated process"));
}

#[test]
fn doctor_ssh_unavailable_skips_remote_probe() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    listen.own(3000, 42);
    let ssh = FakeSsh {
        status: FakeSshStatus::Unavailable,
        ..FakeSsh::ok()
    };
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &ssh);
    assert!(result.is_err());
    assert!(text.contains("SSH: unavailable"));
    assert!(text.contains("unhealthy: SSH to workbox is unavailable"));
    assert!(text.contains("remote=skipped"));
    assert!(!ssh.probed_tcp());
}

#[test]
fn doctor_ssh_auth_skips_remote_probe() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    listen.own(3000, 42);
    let ssh = FakeSsh {
        status: FakeSshStatus::Auth,
        ..FakeSsh::ok()
    };
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &ssh);
    assert!(result.is_err());
    assert!(text.contains("SSH: auth failure"));
    assert!(text.contains("unhealthy: SSH to workbox failed authentication"));
    assert!(!ssh.probed_tcp());
}

#[test]
fn classify_ssh_stderr_fixtures() {
    assert_eq!(
        classify_ssh_stderr(&fixture("doctor/ssh-auth.stderr")),
        SshState::Auth
    );
    assert_eq!(
        classify_ssh_stderr(&fixture("doctor/ssh-unavailable.stderr")),
        SshState::Unavailable
    );
    assert_eq!(
        classify_ssh_stderr("Host key verification failed for workbox"),
        SshState::Auth
    );
    assert_eq!(
        classify_ssh_stderr("ssh: Could not resolve hostname"),
        SshState::Unavailable
    );
}

#[test]
fn paused_mapping_skips_probe_and_does_not_fail_doctor() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(5173, 42);
    listen.own(5173, 42);
    launchd.disabled.borrow_mut().insert(tunnel_label(3000));
    let ssh = FakeSsh::ok();
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &ssh);
    result.unwrap();
    assert!(text.contains("tunnel 3000: supervisor=paused"));
    assert!(!ssh.calls.borrow().iter().any(|call| {
        call.remote_command
            .as_deref()
            .is_some_and(|c| c.contains("/dev/tcp/localhost/3000"))
    }));
}

#[test]
fn probe_transport_failure_is_unknown_not_app_down() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    launchd.start_job(5173, 42);
    listen.own(3000, 42);
    listen.own(5173, 42);
    let ssh = FakeSsh {
        remote: FakeRemote::Failed,
        ..FakeSsh::ok()
    };
    let (result, text) = doctor_text(&doctor_host(), &ctx, &launchd, &listen, &ssh);
    assert!(result.is_err());
    assert!(text.contains("remote=unknown"));
    assert!(!text.contains("remote app is not listening"));
}

#[test]
fn system_ssh_deadline_kills_hanging_child() {
    let home = scratch();
    let ssh_path = home.0.join("ssh");
    fs::write(&ssh_path, "#!/bin/sh\nexec sleep 30\n").unwrap();
    let mut perms = fs::metadata(&ssh_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&ssh_path, perms).unwrap();
    let ssh = SystemSsh::with_program(&ssh_path);
    let start = Instant::now();
    let outcome = ssh.run(
        &SshRequest {
            target: "workbox".into(),
            remote_command: None,
        },
        fast_deadlines(),
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(outcome.code, 124);
}

#[test]
fn doctor_skips_ssh_for_the_current_machine() {
    let home = scratch();
    let maps: Vec<Mapping> = Vec::new();
    let ctx = ctx(&home.0, &maps, Supervisor::None);
    let host = DoctorHost {
        canonical: "laptop".into(),
        ssh_target: "laptop".into(),
        is_local: true,
    };
    let ssh = FakeSsh::ok();
    let (result, text) = doctor_text(
        &host,
        &ctx,
        &FakeLaunchd::new(),
        &FakeListen::default(),
        &ssh,
    );
    result.unwrap();
    assert!(text.contains("SSH: skipped (current machine)"));
    assert!(ssh.calls.borrow().is_empty());
    assert!(text.contains(NO_TUNNELS_CONFIGURED));
}

#[test]
fn doctor_no_mappings_still_checks_ssh() {
    let home = scratch();
    let maps: Vec<Mapping> = Vec::new();
    let ctx = ctx(&home.0, &maps, Supervisor::None);
    let (result, text) = doctor_text(
        &doctor_host(),
        &ctx,
        &FakeLaunchd::new(),
        &FakeListen::default(),
        &FakeSsh::ok(),
    );
    result.unwrap();
    assert!(text.contains("SSH: reachable"));
    assert!(text.contains(NO_TUNNELS_CONFIGURED));
}

#[test]
fn doctor_unsupervised_mapping_is_unhealthy() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::None);
    let (result, text) = doctor_text(
        &doctor_host(),
        &ctx,
        &FakeLaunchd::new(),
        &FakeListen::default(),
        &FakeSsh::ok(),
    );
    assert!(result.is_err());
    assert!(text.contains("supervisor=unsupervised"));
    assert!(text.contains("is not supervised on this host"));
}

#[test]
fn managed_delete_protection() {
    let home = scratch();
    let maps = mappings();
    let launchd_ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    let listen = FakeListen::default();
    launchd.start_job(3000, 42);
    listen.own(3000, 42);
    assert_eq!(
        managed_delete_verdict(42, 3000, &launchd_ctx, &launchd, &listen),
        ManagedDeleteVerdict::Managed { port: 3000 }
    );
    listen.ssh_child(3000, 42, 43);
    assert_eq!(
        managed_delete_verdict(43, 3000, &launchd_ctx, &launchd, &listen),
        ManagedDeleteVerdict::Managed { port: 3000 }
    );
    listen.identities.borrow_mut().remove(&43);
    assert_eq!(
        managed_delete_verdict(43, 3000, &launchd_ctx, &launchd, &listen),
        ManagedDeleteVerdict::Ambiguous { port: 3000 }
    );

    launchd.running.borrow_mut().clear();
    launchd.loaded.borrow_mut().clear();
    launchd.disabled.borrow_mut().insert(tunnel_label(3000));
    listen.pids.borrow_mut().insert(3000, vec![99]);
    assert_eq!(
        managed_delete_verdict(99, 3000, &launchd_ctx, &launchd, &listen),
        ManagedDeleteVerdict::NotManaged
    );

    *launchd.print_unknown.borrow_mut() = true;
    assert_eq!(
        managed_delete_verdict(99, 3000, &launchd_ctx, &launchd, &listen),
        ManagedDeleteVerdict::Ambiguous { port: 3000 }
    );

    let none = ctx(&home.0, &maps, Supervisor::None);
    assert_eq!(
        managed_delete_verdict(42, 3000, &none, &launchd, &listen),
        ManagedDeleteVerdict::NotManaged
    );
}

#[test]
fn unknown_job_read_is_not_treated_as_absence() {
    let home = scratch();
    let maps = mappings();
    let ctx = ctx(&home.0, &maps, Supervisor::Launchd);
    let launchd = FakeLaunchd::new();
    *launchd.print_disabled_fail.borrow_mut() = true;
    launchd.start_job(3000, 42);
    let listen = FakeListen::default();
    listen.own(3000, 42);
    let text = status_text(&ctx, &launchd, &listen);
    assert!(text.contains("unknown"));
    let err = tunnels::resume(3000, &ctx, &launchd, &listen).unwrap_err();
    assert!(matches!(err, TunnelError::JobStateUnknown { .. }));
    assert!(!launchd.has_verb("enable"));
}

#[test]
fn system_launchctl_adapter_records_args_after_preflight() {
    let home = scratch();
    let log = home.0.join("commands");
    fs::write(&log, "").unwrap();
    let bin = home.0.join("launchctl");
    fs::write(
        &bin,
        format!(
            r#"#!/bin/sh
log='{log}'
printf '%s\n' "$*" >>"$log"
if [ "$1" = print-disabled ]; then
  printf '%s\n' 'disabled services = {{'
  printf '%s\n' '}}'
  exit 0
fi
echo "Could not find service $2" >&2
exit 1
"#,
            log = log.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).unwrap();
    let preflight = Command::new(&bin)
        .args(["print-disabled", "gui/501"])
        .output()
        .unwrap();
    assert!(
        preflight.status.success(),
        "launchctl mock could not execute: {}",
        String::from_utf8_lossy(&preflight.stderr)
    );
    assert!(String::from_utf8_lossy(&preflight.stdout).contains("disabled services = {"));
    let backend = SystemLaunchctl::with_program(&bin);
    let out = launchd::launchctl_print_disabled(&backend, "gui/501");
    assert!(out.success());
    let logged = fs::read_to_string(&log).unwrap();
    assert!(logged.contains("print-disabled gui/501"));
}
