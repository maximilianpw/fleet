//! Multi-host commands: status, agents, pick, doctor without HOST, ports,
//! hook, copy -r, and move. SSH is the scripted fake from `common`.

mod common;

use std::fs;

use common::{Fixture, MINIMAL_TOML};

const THREE_HOSTS: &str = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
aliases = []
os = "darwin"
role = "interface"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = false

[hosts.workbox]
ssh_target = "workbox"
aliases = ["dev"]
os = "linux"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = true

[hosts.studio]
ssh_target = "studio"
tailscale_name = "old-studio"
aliases = []
os = "darwin"
role = "compute"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = true
"#;

const TAILNET: &str = r#"{
  "Self": {"HostName": "laptop", "Online": true},
  "Peer": {
    "a": {"HostName": "workbox", "DNSName": "workbox.example.ts.net.", "Online": true, "Active": true, "CurAddr": "192.0.2.10:41641"},
    "b": {"HostName": "old-studio", "DNSName": "old-studio.example.ts.net.", "Online": false, "LastSeen": "2026-10-01T08:30:00Z"}
  }
}"#;

const WORKBOX_AGENT: &str = r#"[{"version":1,"key":"pane-4","agent":"claude","state":"needs-input","task":"fix the build","request":"permission: Bash","session_id":"abc","transcript":"/home/dev/.claude/projects/-home-dev-src-app/abc.jsonl","cwd":"/home/dev/src/app","tmux_session":"agents","tmux_pane":"%4","updated_at":1}]"#;

fn probe(agents: &str) -> String {
    format!(
        "@fleet\nfleet {}\n@tmux\nok\nmain|1|2\nagents|0|1\n@agents\n{agents}\n@end\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn three_host_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write_xdg_config(THREE_HOSTS);
    fixture.set_tailscale(TAILNET);
    fixture
}

#[test]
fn status_reports_presence_sessions_and_agents_and_skips_offline_hosts() {
    let fixture = three_host_fixture();
    fixture.respond("workbox", "fleet-probe", &probe(WORKBOX_AGENT), 0);
    let output = fixture.fleet_scripted().arg("status").output().unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    let row = |host: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(host))
            .unwrap_or_else(|| panic!("no {host} row in\n{stdout}"))
            .to_string()
    };
    assert!(row("laptop").contains("self"));
    let workbox = row("workbox ");
    assert!(workbox.contains("online"), "{workbox}");
    assert!(workbox.contains("direct"), "{workbox}");
    assert!(workbox.contains("main*,agents"), "{workbox}");
    assert!(workbox.contains("▲ needs you 1"), "{workbox}");
    let studio = row("studio");
    assert!(studio.contains("offline"), "{studio}");
    assert!(studio.contains("seen 2026-10-01 08:30"), "{studio}");
    assert!(stdout.contains("permission: Bash"));
    assert_eq!(fixture.ssh_calls(), ["workbox fleet-probe"]);
}

#[test]
fn status_json_and_where_filter() {
    let fixture = three_host_fixture();
    fixture.respond("workbox", "fleet-probe", &probe("[]"), 0);
    let output = fixture
        .fleet_scripted()
        .args(["status", "--where", "online,!local", "--json"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let hosts = value["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0]["host"], "workbox");
    assert_eq!(hosts[0]["presence"]["state"], "online");
    assert_eq!(hosts[0]["sessions"][0]["name"], "main");
}

#[test]
fn unreachable_and_fleetless_hosts_are_reported_not_fatal() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fixture.respond("workbox", "fleet-probe", "", 255);
    fs::write(
        fixture.responses().join("workbox.fleet-probe.err"),
        "Permission denied (publickey).\n",
    )
    .unwrap();
    let output = fixture.fleet_scripted().arg("status").output().unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 0);
    assert!(stdout.contains("ssh auth failure"), "{stdout}");

    fixture.respond(
        "workbox",
        "fleet-probe",
        "@fleet\nmissing\n@tmux\nok\n@agents\n@end\n",
        0,
    );
    fs::remove_file(fixture.responses().join("workbox.fleet-probe.err")).unwrap();
    let output = fixture.fleet_scripted().arg("agents").output().unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("workbox: fleet is not on the remote PATH"),
        "{stdout}"
    );
}

#[test]
fn pick_selects_by_metadata() {
    let fixture = three_host_fixture();
    let pick = |args: &[&str]| {
        let output = fixture
            .fleet_scripted()
            .arg("pick")
            .args(args)
            .output()
            .unwrap();
        Fixture::output_text(&output)
    };
    assert_eq!(pick(&["--where", "long_running_agents"]).0, "studio\n");
    assert_eq!(pick(&["--where", "os=linux"]).0, "workbox\n");
    assert_eq!(pick(&["--where", "online,!local"]).0, "workbox\n");
    assert_eq!(
        pick(&["--where", "long_running_agents", "--all"]).0,
        "studio\nworkbox\n"
    );
    let (_, stderr, code) = pick(&["--where", "os=plan9"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no host matches"));
    let (_, stderr, code) = pick(&["--where", "colour=blue"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown --where key"));
}

#[test]
fn doctor_without_host_checks_every_host() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fixture.respond("workbox", "fleet-probe", &probe("[]"), 0);
    let output = fixture.fleet_scripted().arg("doctor").output().unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("== laptop ==\nSSH: skipped (current machine)"));
    assert!(stdout.contains("== workbox ==\nSSH: reachable"));
    assert!(stdout.contains("tmux: ok"));
    assert!(stdout.contains(&format!("fleet: {}", env!("CARGO_PKG_VERSION"))));

    fixture.respond(
        "workbox",
        "fleet-probe",
        "@fleet\nmissing\n@tmux\nmissing\n@agents\n@end\n",
        0,
    );
    let output = fixture
        .fleet_scripted()
        .args(["doctor", "workbox", "--json"])
        .output()
        .unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 1);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["host"], "workbox");
    assert_eq!(value["healthy"], false);
    let kinds: Vec<&str> = value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"unhealthy") && kinds.contains(&"warning"),
        "{kinds:?}"
    );
}

#[test]
fn doctor_single_host_text_keeps_historical_lines_first() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fixture.respond("workbox", "fleet-probe", &probe("[]"), 0);
    let output = fixture
        .fleet_scripted()
        .args(["doctor", "dev"])
        .output()
        .unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "SSH: reachable");
    assert_eq!(lines[1], "No managed tunnels configured on this host.");
    assert_eq!(lines[2], "tmux: ok");
}

#[test]
fn list_and_tunnel_status_json() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture.fleet().args(["list", "--json"]).output().unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 0);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["current_host"], "laptop");
    assert_eq!(value["hosts"][1]["name"], "workbox");
    assert_eq!(value["hosts"][1]["long_running_agents"], true);

    let output = fixture
        .fleet()
        .args(["tunnel", "status", "--json"])
        .output()
        .unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[]");

    fixture.set_ps("4242 ssh -N -L 127.0.0.1:3000:localhost:3000 workbox\n");
    let output = fixture
        .fleet()
        .args(["forward", "list", "--json"])
        .output()
        .unwrap();
    let (stdout, _, code) = Fixture::output_text(&output);
    assert_eq!(code, 0);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value[0]["pid"], 4242);
    assert_eq!(value[0]["local_port"], 3000);
}

#[test]
fn ports_lists_listeners_from_ss() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    fixture.respond(
        "workbox",
        "fleet-ports",
        "@ss\nLISTEN 0 4096 127.0.0.1:5173 0.0.0.0:* users:((\"node\",pid=812,fd=21))\n",
        0,
    );
    let output = fixture
        .fleet_scripted()
        .args(["ports", "dev"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("5173    127.0.0.1"), "{stdout}");
    assert!(stdout.contains("node (812)"));
    assert!(stdout.contains("fleet forward workbox 5173 5173"));
}

#[test]
fn copy_recursive_passes_scp_r() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture
        .fleet()
        .args(["copy", "-r", "build", "workbox:/tmp/build"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        fixture.scp_args().unwrap(),
        ["-r", "--", "build", "workbox:/tmp/build"]
    );
}

/// Fake tmux that reports pane `%9` alive and names its session `agents`.
const PANE_TMUX: &str = r#"#!/bin/sh
case "$1" in
  display-message) printf 'agents\n' ;;
  list-panes) printf '%%9\n' ;;
esac
exit 0
"#;

#[test]
fn hook_reports_state_that_agents_lists() {
    let fixture = Fixture::new();
    fixture.write_bin("tmux", PANE_TMUX);
    let hook = |args: &[&str], stdin: &str| {
        let mut child = fixture
            .fleet_scripted()
            .env("TMUX_PANE", "%9")
            .arg("hook")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        Fixture::output_text(&child.wait_with_output().unwrap())
    };
    let (_, stderr, code) = hook(
        &[
            "set",
            "--state",
            "running",
            "--agent",
            "claude",
            "--from-hook-json",
        ],
        r#"{"session_id":"s1","prompt":"ship the release","cwd":"/repo","transcript_path":"/t.jsonl"}"#,
    );
    assert_eq!(code, 0, "{stderr}");

    let local = fixture
        .fleet_scripted()
        .args(["agents", "--local", "--json"])
        .output()
        .unwrap();
    let (stdout, _, code) = Fixture::output_text(&local);
    assert_eq!(code, 0);
    let records: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(records[0]["state"], "running");
    assert_eq!(records[0]["tmux_session"], "agents");
    assert_eq!(records[0]["task"], "ship the release");
    assert_eq!(records[0]["transcript"], "/t.jsonl");

    let (_, stderr, code) = hook(&["set", "--state", "sleeping"], "");
    assert_eq!(code, 1, "bad hook input must not exit 2");
    assert!(stderr.contains("unknown state"));

    let (_, _, code) = hook(&["clear"], "");
    assert_eq!(code, 0);
    let local = fixture
        .fleet_scripted()
        .args(["agents", "--local", "--json"])
        .output()
        .unwrap();
    assert_eq!(Fixture::output_text(&local).0.trim(), "[]");
}

fn move_fixture(agent_state: &str) -> Fixture {
    let fixture = three_host_fixture();
    fixture.respond(
        "workbox",
        "fleet-agents",
        &WORKBOX_AGENT.replace("needs-input", agent_state),
        0,
    );
    fixture.respond(
        "workbox",
        "fleet-git-source",
        "top=/home/dev/src/app\nbranch=feature/x\nremote=origin\nurl=git@example.test:dev/app.git\nhome=/home/dev\n",
        0,
    );
    fixture.respond("studio", "fleet-target-home", "home=/Users/dev\n", 0);
    fixture.respond("studio", "fleet-target-prep", "ok\n", 0);
    fixture.respond("workbox", "fleet-transcript-read", "{\"line\":1}\n", 0);
    fixture.respond("studio", "fleet-start", "ok\n", 0);
    fixture
}

#[test]
fn move_resumes_the_conversation_on_the_target_then_ends_the_source() {
    let fixture = move_fixture("done");
    let output = fixture
        .fleet_scripted()
        .args(["move", "dev", "agents", "studio"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let calls = fixture.ssh_calls();
    let position = |call: &str| {
        calls
            .iter()
            .position(|line| line == call)
            .unwrap_or_else(|| panic!("missing {call} in {calls:?}"))
    };
    assert!(position("studio fleet-target-prep") < position("studio fleet-transcript-write"));
    assert!(position("studio fleet-transcript-write") < position("studio fleet-start"));
    assert!(position("studio fleet-start") < position("workbox fleet-stop"));

    assert_eq!(
        fixture
            .response_file("studio", "fleet-transcript-write", "stdin")
            .unwrap(),
        "{\"line\":1}\n"
    );
    let write = fixture
        .response_file("studio", "fleet-transcript-write", "cmd")
        .unwrap();
    assert!(
        write.contains("/Users/dev/.claude/projects/-Users-dev-src-app/abc.jsonl"),
        "{write}"
    );
    let start = fixture
        .response_file("studio", "fleet-start", "cmd")
        .unwrap();
    assert!(start.contains("claude --resume abc"), "{start}");
    assert!(start.contains("/Users/dev/src/app"), "{start}");
    let prep = fixture
        .response_file("studio", "fleet-target-prep", "cmd")
        .unwrap();
    assert!(prep.contains("git@example.test:dev/app.git"), "{prep}");
    assert!(stdout.contains("attach with: fleet ssh studio agents"));
}

#[test]
fn move_refuses_busy_agents_and_unpushed_work_before_touching_the_target() {
    let fixture = move_fixture("running");
    let output = fixture
        .fleet_scripted()
        .args(["move", "workbox", "agents", "studio"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 1);
    assert!(stderr.contains("--force"), "{stderr}");
    assert!(fixture
        .ssh_calls()
        .iter()
        .all(|call| !call.starts_with("studio")));

    let fixture = move_fixture("done");
    fixture.respond("workbox", "fleet-git-source", "", 15);
    let output = fixture
        .fleet_scripted()
        .args(["move", "workbox", "agents", "studio", "--keep"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 1);
    assert!(stderr.contains("not pushed"), "{stderr}");
    assert!(fixture
        .ssh_calls()
        .iter()
        .all(|call| !call.starts_with("studio")));

    let output = fixture
        .fleet_scripted()
        .args(["move", "workbox", "agents", "dev"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "same host via alias");
}

#[test]
fn move_keeps_source_when_the_target_session_dies() {
    let fixture = move_fixture("idle");
    fixture.respond("studio", "fleet-start", "", 41);
    let output = fixture
        .fleet_scripted()
        .args(["move", "workbox", "agents", "studio"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 1);
    assert!(stderr.contains("exited right away"), "{stderr}");
    assert!(!fixture
        .ssh_calls()
        .contains(&"workbox fleet-stop".to_string()));
}

/// Every remote script must reach `sh` byte-for-byte through a POSIX or fish
/// login shell. Replacing the leading `sh -c` with `printf %s` echoes what
/// `sh` would have received.
#[test]
fn wrapped_scripts_survive_posix_and_fish_login_shells() {
    use fleet::remote::wrap_for_login_shell;
    use std::process::Command;

    let scripts = [
        fleet::status::remote_probe_script("/run/current-system/sw/bin/tmux"),
        fleet::ports::ports_script(),
        fleet::moving::source_repo_script("/home/dev/it's here"),
        fleet::moving::target_prep_script(
            "/Users/dev/it's here",
            "git@example.test:dev/app.git",
            "origin",
            "feature/x",
            "tmux",
            "agents",
        ),
        fleet::moving::start_script("tmux", "agents", "/Users/dev/app", "claude --resume abc"),
    ];
    for script in scripts {
        let wrapped = wrap_for_login_shell(&script);
        let echo = format!("printf %s {}", wrapped.strip_prefix("sh -c ").unwrap());
        let posix = Command::new("sh").args(["-c", &echo]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&posix.stdout), script);
        let syntax = Command::new("sh")
            .args(["-n", "-c", &script])
            .status()
            .unwrap();
        assert!(syntax.success(), "sh rejected: {script}");
        match Command::new("fish")
            .args(["--no-config", "-c", &echo])
            .output()
        {
            Ok(fish) => assert_eq!(String::from_utf8_lossy(&fish.stdout), script),
            Err(error) if std::env::var_os("FLEET_REQUIRE_FISH").is_some() => {
                panic!("fish is required for this test: {error}")
            }
            Err(_) => eprintln!("skipping fish quoting check: fish is unavailable"),
        }
    }
}

#[test]
fn move_does_not_end_a_source_agent_that_is_still_busy() {
    let fixture = move_fixture("running");
    let output = fixture
        .fleet_scripted()
        .args(["move", "workbox", "agents", "studio", "--force"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("both sessions are running"), "{stdout}");
    assert!(fixture
        .ssh_calls()
        .contains(&"studio fleet-start".to_string()));
    assert!(!fixture
        .ssh_calls()
        .contains(&"workbox fleet-stop".to_string()));
}

#[test]
fn move_fails_fast_when_the_transcript_writer_dies() {
    let fixture = move_fixture("done");
    let large = "x".repeat(512 * 1024);
    fixture.respond("workbox", "fleet-transcript-read", &large, 0);
    fixture.respond("studio", "fleet-transcript-write", "", 1);
    fs::write(
        fixture
            .responses()
            .join("studio.fleet-transcript-write.err"),
        "mkdir: permission denied\n",
    )
    .unwrap();
    let started = std::time::Instant::now();
    let output = fixture
        .fleet_scripted()
        .args(["move", "workbox", "agents", "studio"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(code, 1);
    assert!(stderr.contains("permission denied"), "{stderr}");
    assert!(!fixture
        .ssh_calls()
        .contains(&"studio fleet-start".to_string()));
}
