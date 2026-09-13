mod common;

use std::collections::BTreeMap;
use std::fs;
use std::process::Stdio;
use std::time::{Duration, Instant};

use fleet::config::parse_config;
use fleet::forwards::{
    collect_forward_rows, ensure_ports_free, forward_local_port, parse_forward_pids,
    render_forward_list, stop_forwards, ForwardError, ManagedClass, SnapshotInspector,
};
use fleet::process::{
    parse_ps_output, ObservedProcess, ProcessEnv, RecordingSignals, SnapshotProcesses,
};
use fleet::ssh::{
    parse_ssh_tail, plan_ad_hoc_forward, plan_run, plan_shell, plan_ssh, plan_t3,
    AttachmentForward, SshError,
};
use fleet::PlannedCommand;

use common::{Fixture, MINIMAL_TOML, NIX_STYLE_TOML};

fn arg_strs(command: &PlannedCommand) -> Vec<&str> {
    command.args.iter().map(String::as_str).collect()
}

fn env_with_shell() -> ProcessEnv {
    ProcessEnv {
        shell: Some("/tmp/fakeshell".into()),
        ..ProcessEnv::default()
    }
}

#[test]
fn ssh_with_tmux_target_uses_that_alias() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let planned = plan_ssh(&config, "workbox", None, &[]).unwrap();
    assert_eq!(planned.program, "ssh");
    assert_eq!(arg_strs(&planned), ["tm-workbox"]);
}

#[test]
fn ssh_named_session_uses_explicit_remote_tmux_and_tty() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let planned = plan_ssh(&config, "workbox", Some("agents"), &[]).unwrap();
    assert_eq!(planned.program, "ssh");
    assert_eq!(
        arg_strs(&planned),
        [
            "-t",
            "workbox",
            "/run/current-system/sw/bin/tmux new-session -A -s 'agents'",
        ]
    );
}

#[test]
fn ssh_named_session_repeatable_forwards_normalize_loopback() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let forwards = vec![
        AttachmentForward::parse("3000").unwrap(),
        AttachmentForward::parse("4000:5000").unwrap(),
    ];
    let planned = plan_ssh(&config, "workbox", Some("agents"), &forwards).unwrap();
    assert_eq!(
        arg_strs(&planned),
        [
            "-t",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-L",
            "127.0.0.1:3000:localhost:3000",
            "-L",
            "127.0.0.1:4000:localhost:5000",
            "workbox",
            "/run/current-system/sw/bin/tmux new-session -A -s 'agents'",
        ]
    );
}

#[test]
fn ssh_default_session_with_forward_keeps_tmux_target() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let forwards = vec![AttachmentForward::parse("5173").unwrap()];
    let planned = plan_ssh(&config, "workbox", None, &forwards).unwrap();
    assert_eq!(
        arg_strs(&planned),
        [
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-L",
            "127.0.0.1:5173:localhost:5173",
            "tm-workbox",
        ]
    );
}

#[test]
fn ssh_without_tmux_target_builds_explicit_remote_tmux() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_ssh(&config, "workbox", None, &[]).unwrap();
    assert_eq!(
        arg_strs(&planned),
        ["-t", "workbox", "tmux new-session -A -s 'main'"]
    );
}

#[test]
fn known_alias_keeps_nix_target_triple() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let ssh = plan_ssh(&config, "dev", None, &[]).unwrap();
    assert_eq!(arg_strs(&ssh), ["tm-dev"]);
    let named = plan_ssh(&config, "dev", Some("agents"), &[]).unwrap();
    assert_eq!(named.args[1], "dev");
    let forward = plan_ad_hoc_forward(&config, "dev", 3000, 3000, "localhost").unwrap();
    assert_eq!(forward.args.last().unwrap(), "fleet-forward-dev");
}

#[test]
fn unknown_alias_uses_legacy_pass_through() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let ssh = plan_ssh(&config, "ghost", None, &[]).unwrap();
    assert_eq!(arg_strs(&ssh), ["tm-ghost"]);
    let shell = plan_shell(
        &config,
        "ghost",
        &["-o".into(), "RequestTTY=yes".into()],
        &env_with_shell(),
    )
    .unwrap();
    assert_eq!(arg_strs(&shell), ["ghost", "-o", "RequestTTY=yes"]);
    let run = plan_run(&config, "ghost", &["btop".into()]).unwrap();
    assert_eq!(arg_strs(&run), ["ghost", "btop"]);
    let forward = plan_ad_hoc_forward(&config, "ghost", 3000, 3000, "localhost").unwrap();
    assert_eq!(forward.args.last().unwrap(), "fleet-forward-ghost");
    match plan_t3(&config, "ghost", None) {
        Err(SshError::UnknownHost(host)) => assert_eq!(host, "ghost"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn local_ssh_uses_path_tmux_and_main_default() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let planned = plan_ssh(&config, "laptop", None, &[]).unwrap();
    assert_eq!(planned.program, "tmux");
    assert_eq!(arg_strs(&planned), ["new-session", "-A", "-s", "main"]);
    let portable = plan_ssh(&config, "portable", Some("local"), &[]).unwrap();
    assert_eq!(arg_strs(&portable), ["new-session", "-A", "-s", "local"]);
}

#[test]
fn local_ssh_rejects_forwards() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let forwards = vec![AttachmentForward::parse("3000").unwrap()];
    match plan_ssh(&config, "laptop", None, &forwards) {
        Err(SshError::ForwardOnLocalHost) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn unsafe_session_is_rejected_before_planning_ssh() {
    match parse_ssh_tail(&["invalid/session".into()]) {
        Err(SshError::InvalidSession) => {}
        other => panic!("unexpected {other:?}"),
    }
    match AttachmentForward::parse("3000:invalid") {
        Err(SshError::Port(_)) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn option_like_host_is_rejected() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    match plan_ssh(&config, "-oBatchMode=yes", None, &[]) {
        Err(SshError::Target(_)) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn shell_local_ignores_extras_and_uses_shell() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_shell(
        &config,
        "laptop",
        &["-o".into(), "Foo=bar".into()],
        &env_with_shell(),
    )
    .unwrap();
    assert_eq!(planned.program, "/tmp/fakeshell");
    assert!(planned.args.is_empty());
}

#[test]
fn shell_remote_keeps_trailing_ssh_argv() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_shell(
        &config,
        "workbox",
        &["-o".into(), "RequestTTY=yes".into()],
        &env_with_shell(),
    )
    .unwrap();
    assert_eq!(planned.program, "ssh");
    assert_eq!(arg_strs(&planned), ["workbox", "-o", "RequestTTY=yes"]);
}

#[test]
fn run_local_preserves_argv_including_spaces() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_run(
        &config,
        "laptop",
        &["dummycmd".into(), "hello world".into(), "-n".into()],
    )
    .unwrap();
    assert_eq!(planned.program, "dummycmd");
    assert_eq!(arg_strs(&planned), ["hello world", "-n"]);
}

#[test]
fn run_remote_passes_argv_to_ssh_for_remote_shell_joining() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_run(&config, "workbox", &["echo".into(), "hello world".into()]).unwrap();
    assert_eq!(planned.program, "ssh");
    assert_eq!(arg_strs(&planned), ["workbox", "echo", "hello world"]);
}

#[test]
fn run_remote_preserves_caller_quoting_for_openssh_and_fish() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_run(
        &config,
        "workbox",
        &["printf".into(), "'%s\\n'".into(), "'hello world'".into()],
    )
    .unwrap();
    assert_eq!(
        arg_strs(&planned),
        ["workbox", "printf", "'%s\\n'", "'hello world'"]
    );
    let remote_command = arg_strs(&planned)[1..].join(" ");
    match std::process::Command::new("fish")
        .args(["--no-config", "--no-execute", "-c", &remote_command])
        .status()
    {
        Ok(status) => assert!(
            status.success(),
            "fish rejected run command: {remote_command}"
        ),
        Err(error) if std::env::var_os("FLEET_REQUIRE_FISH").is_some() => {
            panic!("fish is required for this test: {error}")
        }
        Err(_) => eprintln!("skipping fish run-command parse test: fish is unavailable"),
    }
}

#[test]
fn t3_uses_declared_port_and_optional_local_override() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let planned = plan_t3(&config, "workbox", None).unwrap();
    assert!(planned.args.contains(&"-N".into()));
    assert!(planned.args.contains(&"ForwardAgent=no".into()));
    assert_eq!(
        planned.args[planned.args.len() - 2],
        "127.0.0.1:51001:127.0.0.1:51001"
    );
    assert_eq!(planned.args.last().unwrap(), "fleet-forward-workbox");
    let override_local = plan_t3(&config, "workbox", Some(51002)).unwrap();
    assert_eq!(
        override_local.args[override_local.args.len() - 2],
        "127.0.0.1:51002:127.0.0.1:51001"
    );
}

#[test]
fn t3_rejects_host_without_port_metadata() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    match plan_t3(&config, "workbox", None) {
        Err(SshError::MissingT3Port(host)) => assert_eq!(host, "workbox"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn ad_hoc_forward_passes_bracketed_remote_host_as_data() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_ad_hoc_forward(&config, "workbox", 3000, 4000, "[::1]").unwrap();
    assert_eq!(
        planned.args[planned.args.len() - 2],
        "127.0.0.1:3000:[::1]:4000"
    );
    assert_eq!(planned.args.last().unwrap(), "workbox");
}

#[test]
fn plain_ssh_alias_uses_ssh_target_not_generated_forward_alias() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_ad_hoc_forward(&config, "workbox", 3000, 3000, "localhost").unwrap();
    assert_eq!(planned.args.last().unwrap(), "workbox");
}

#[test]
fn binary_ssh_and_local_tmux_exec_fake_commands() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(NIX_STYLE_TOML);

    let output = fixture.fleet().args(["ssh", "workbox"]).output().unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(fixture.ssh_args(), Some(vec!["tm-workbox".into()]));
    assert!(fixture.tmux_args().is_none());

    let output = fixture
        .fleet()
        .args(["ssh", "workbox", "agents"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.ssh_args(),
        Some(vec![
            "-t".into(),
            "workbox".into(),
            "/run/current-system/sw/bin/tmux new-session -A -s 'agents'".into(),
        ])
    );

    let output = fixture
        .fleet()
        .args([
            "ssh",
            "workbox",
            "agents",
            "--forward",
            "3000",
            "--forward",
            "4000:5000",
        ])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.ssh_args(),
        Some(vec![
            "-t".into(),
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPath=none".into(),
            "-L".into(),
            "127.0.0.1:3000:localhost:3000".into(),
            "-L".into(),
            "127.0.0.1:4000:localhost:5000".into(),
            "workbox".into(),
            "/run/current-system/sw/bin/tmux new-session -A -s 'agents'".into(),
        ])
    );

    let output = fixture
        .fleet()
        .args(["ssh", "laptop", "local"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.tmux_args(),
        Some(vec![
            "new-session".into(),
            "-A".into(),
            "-s".into(),
            "local".into(),
        ])
    );
}

#[test]
fn binary_rejects_unsafe_session_and_local_forward_without_spawning() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture
        .fleet()
        .args(["ssh", "workbox", "invalid/session"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("session names may only contain"));
    assert!(!fixture.ssh_log.exists());
    assert!(!fixture.tmux_log.exists());

    let output = fixture
        .fleet()
        .args(["ssh", "workbox", "--forward", "3000:invalid"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(!fixture.ssh_log.exists());

    let output = fixture
        .fleet()
        .args(["ssh", "laptop", "--forward", "3000"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--forward requires a remote Fleet host"));
    assert!(!fixture.ssh_log.exists());
    assert!(!fixture.tmux_log.exists());
}

#[test]
fn binary_shell_and_run_paths() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);

    let output = fixture
        .fleet()
        .args(["shell", "workbox", "-o", "RequestTTY=yes"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.ssh_args(),
        Some(vec!["workbox".into(), "-o".into(), "RequestTTY=yes".into()])
    );

    let output = fixture
        .fleet()
        .args(["shell", "laptop", "-o", "RequestTTY=yes"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    let shell_log = fixture.shell_args().expect("shell ran");
    assert!(shell_log.iter().any(|line| line.starts_with("program=")));
    assert!(!shell_log.iter().any(|line| line == "-o"));

    let output = fixture
        .fleet()
        .args(["run", "laptop", "dummycmd", "hello world", "-n"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.dummy_args(),
        Some(vec!["hello world".into(), "-n".into()])
    );

    let output = fixture
        .fleet()
        .args(["run", "workbox", "echo", "hello world"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.ssh_args(),
        Some(vec!["workbox".into(), "echo".into(), "hello world".into()])
    );
}

#[test]
fn binary_t3_and_forward_create() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(NIX_STYLE_TOML);
    let output = fixture.fleet().args(["t3", "workbox"]).output().unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    let args = fixture.ssh_args().expect("t3 ssh");
    assert!(args.contains(&"-N".into()));
    assert!(args.contains(&"127.0.0.1:51001:127.0.0.1:51001".into()));
    assert_eq!(args.last().unwrap(), "fleet-forward-workbox");

    let output = fixture
        .fleet()
        .args(["forward", "workbox", "3000", "4000", "[::1]"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    let args = fixture.ssh_args().expect("forward ssh");
    assert!(args.contains(&"127.0.0.1:3000:[::1]:4000".into()));
    assert_eq!(args.last().unwrap(), "fleet-forward-workbox");
}

#[test]
fn forward_discovery_supports_joined_and_split_dash_l() {
    let processes = parse_ps_output(
        " 111 ssh -N -L 127.0.0.1:3000:localhost:3000 fleet-forward-workbox\n\
         222 /usr/bin/ssh -n -L127.0.0.1:4000:localhost:4000 other\n\
         333 bash -c ssh\n\
         notapid ssh -L 127.0.0.1:5000:localhost:5000 x\n",
    );
    let rows = collect_forward_rows(&processes);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].pid, 111);
    assert_eq!(rows[0].local_port, 3000);
    assert_eq!(rows[0].spec, "127.0.0.1:3000:localhost:3000");
    assert_eq!(rows[1].pid, 222);
    assert_eq!(rows[1].local_port, 4000);
    assert_eq!(forward_local_port("3000:localhost:3000"), Some(3000));
    assert_eq!(
        forward_local_port("127.0.0.1:5173:localhost:5173"),
        Some(5173)
    );
}

#[test]
fn forward_list_and_delete_aliases_use_fake_ps_and_kill() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fixture.set_ps("424242 ssh -N -L 127.0.0.1:3000:localhost:3000 workbox\n");

    let listed = fixture.fleet().args(["forward", "list"]).output().unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&listed);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Active SSH local forwards:"));
    assert!(stdout.contains("424242"));
    assert!(stdout.contains("fleet forward delete 424242"));

    let listed_alias = fixture
        .fleet()
        .args(["forward", "ls", "3000"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&listed_alias);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Active SSH local forwards for port 3000:"));

    let deleted = fixture
        .fleet()
        .args(["forward", "delete", "424242"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&deleted);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("fleet: stopped SSH forward process 424242"));
    assert_eq!(fixture.kill_args(), Some(vec!["424242".into()]));

    fixture.set_ps("424242 ssh -N -L 127.0.0.1:3000:localhost:3000 workbox\n");
    let stopped = fixture
        .fleet()
        .args(["forward", "rm", "424242"])
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(0));
}

#[test]
fn port_conflict_is_detected_from_observed_forwards() {
    let processes = vec![ObservedProcess {
        pid: 9,
        argv: vec![
            "ssh".into(),
            "-L".into(),
            "127.0.0.1:3000:localhost:3000".into(),
            "workbox".into(),
        ],
    }];
    match ensure_ports_free(&[3000], &processes) {
        Err(ForwardError::PortBusy(message)) => {
            assert!(message.contains("local port 3000 already has an active SSH forward"));
            assert!(message.contains("fleet forward delete 9"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn missing_ps_is_reported_only_when_forward_inspection_is_needed() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fs::remove_file(fixture.bin.join("ps")).unwrap();

    let plain = fixture
        .fleet()
        .env("PATH", &fixture.bin)
        .args(["ssh", "workbox"])
        .output()
        .unwrap();
    assert!(plain.status.success(), "plain ssh should not require ps");

    let forwarded = fixture
        .fleet()
        .env("PATH", &fixture.bin)
        .args(["ssh", "workbox", "--forward", "5173"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&forwarded);
    assert_eq!(code, 1);
    assert!(stderr.contains("missing command `ps`"), "{stderr}");
    assert!(stderr.contains("list SSH forwards"), "{stderr}");
}

#[test]
fn binary_port_conflict_does_not_exec_ssh() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fixture.set_ps("9 ssh -L 127.0.0.1:3000:localhost:3000 workbox\n");
    let output = fixture
        .fleet()
        .args(["forward", "workbox", "3000", "3000"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("already has an active SSH forward"));
    assert!(!fixture.ssh_log.exists());
}

#[test]
fn unmanaged_delete_reobserves_and_records_signal() {
    let table = SnapshotProcesses {
        rows: vec![ObservedProcess {
            pid: 77,
            argv: vec![
                "ssh".into(),
                "-L".into(),
                "127.0.0.1:3000:localhost:3000".into(),
                "workbox".into(),
            ],
        }],
    };
    let inspector = SnapshotInspector {
        class_by_pid: BTreeMap::new(),
        default: ManagedClass::Unmanaged,
    };
    let signals = RecordingSignals::default();
    let stopped = stop_forwards(&[77], &table, &inspector, &signals).unwrap();
    assert_eq!(stopped, vec![77]);
    assert_eq!(*signals.pids.lock().unwrap(), vec![77]);
}

#[test]
fn verified_managed_delete_is_refused_with_pause_guidance() {
    let table = SnapshotProcesses {
        rows: vec![ObservedProcess {
            pid: 88,
            argv: vec![
                "ssh".into(),
                "-L".into(),
                "127.0.0.1:5173:localhost:5173".into(),
                "workbox".into(),
            ],
        }],
    };
    let inspector = SnapshotInspector {
        class_by_pid: BTreeMap::from([(
            88,
            ManagedClass::Managed {
                port: 5173,
                label: "org.nix-community.home.fleet-tunnel-5173".into(),
            },
        )]),
        default: ManagedClass::Unmanaged,
    };
    let signals = RecordingSignals::default();
    match stop_forwards(&[88], &table, &inspector, &signals) {
        Err(ForwardError::ManagedPid { pid, port }) => {
            assert_eq!(pid, 88);
            assert_eq!(port, 5173);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(signals.pids.lock().unwrap().is_empty());
}

#[test]
fn launchd_mapped_port_without_snapshot_is_ambiguous() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let table = SnapshotProcesses {
        rows: vec![ObservedProcess {
            pid: 99,
            argv: vec![
                "ssh".into(),
                "-L".into(),
                "127.0.0.1:5173:localhost:5173".into(),
                "workbox".into(),
            ],
        }],
    };
    let inspector = fleet::managed_inspector_for(&config);
    let signals = RecordingSignals::default();
    match stop_forwards(&[99], &table, &inspector, &signals) {
        Err(ForwardError::AmbiguousManaged { pid }) => assert_eq!(pid, 99),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn supervisor_none_does_not_fabricate_managed_ownership() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let table = SnapshotProcesses {
        rows: vec![ObservedProcess {
            pid: 99,
            argv: vec![
                "ssh".into(),
                "-L".into(),
                "127.0.0.1:5173:localhost:5173".into(),
                "workbox".into(),
            ],
        }],
    };
    let inspector = fleet::managed_inspector_for(&config);
    let signals = RecordingSignals::default();
    let stopped = stop_forwards(&[99], &table, &inspector, &signals).unwrap();
    assert_eq!(stopped, vec![99]);
}

#[test]
fn parse_forward_pids_matches_legacy_messages() {
    match parse_forward_pids(&[]) {
        Err(ForwardError::MissingPids) => {}
        other => panic!("unexpected {other:?}"),
    }
    match parse_forward_pids(&["abc".into()]) {
        Err(ForwardError::PidNotNumeric(value)) => assert_eq!(value, "abc"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn empty_forward_list_message() {
    let text = render_forward_list(&[], None);
    assert!(text.contains("No active SSH local forwards found."));
}

#[test]
fn exec_replaces_process_for_signal_delivery() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let mut child = fixture
        .fleet()
        .env("FLEET_SSH_HOLD", "1")
        .args(["ssh", "workbox", "agents"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fleet");
    let started = Instant::now();
    while !fixture.ssh_ready.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "fake ssh never became ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = child.id();
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("kill TERM");
    assert!(status.success());
    let wait_status = child.wait().expect("wait fleet");
    assert!(wait_status.success() || wait_status.code() == Some(0) || wait_status.code().is_none());
    let signal = std::fs::read_to_string(&fixture.ssh_signal).expect("signal log");
    assert_eq!(signal.trim(), "TERM");
}

#[test]
fn pty_marks_ssh_stdin_as_tty_when_script_is_available() {
    let gnu_script = std::process::Command::new("script")
        .args(["-q", "-c", "true", "/dev/null"])
        .status()
        .is_ok_and(|status| status.success());
    let bsd_script = !gnu_script
        && std::process::Command::new("script")
            .args(["-q", "/dev/null", "/usr/bin/true"])
            .status()
            .is_ok_and(|status| status.success());
    if !gnu_script && !bsd_script {
        eprintln!("skipping PTY test: no supported script(1) interface");
        return;
    }
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let fleet = env!("CARGO_BIN_EXE_fleet");
    let quoted = format!(
        "HOME='{home}' XDG_CONFIG_HOME='{xdg}' PATH='{path}' SHELL='{shell}' \
         FLEET_SSH_ARGS_LOG='{ssh_log}' FLEET_SSH_META_LOG='{meta}' \
         FLEET_PS_OUTPUT='{ps}' '{fleet}' ssh workbox agents",
        home = fixture.home.display(),
        xdg = fixture.xdg.display(),
        path = fixture.path(),
        shell = fixture.bin.join("fakeshell").display(),
        ssh_log = fixture.ssh_log.display(),
        meta = fixture.ssh_meta.display(),
        ps = fixture.ps_output.display(),
        fleet = fleet,
    );
    let mut command = std::process::Command::new("script");
    if gnu_script {
        command.args(["-q", "-c", &quoted, "/dev/null"]);
    } else {
        command.args(["-q", "/dev/null", "/bin/sh", "-c", &quoted]);
    }
    let output = command.output().expect("script");
    assert!(
        output.status.success(),
        "fleet failed under a PTY: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let meta = std::fs::read_to_string(&fixture.ssh_meta).expect("ssh meta");
    assert!(
        meta.contains("stdin_tty=yes"),
        "expected tty stdin, got {meta}"
    );
}

#[test]
fn doctor_unknown_host_is_rejected_without_ssh() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture.fleet().args(["doctor", "ghost"]).output().unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("unknown Fleet host: ghost"));
    assert!(!fixture.ssh_log.exists());
}
