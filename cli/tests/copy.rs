mod common;

use fleet::config::parse_config;
use fleet::copy::{plan_copy, CopyError};

use common::{Fixture, MINIMAL_TOML, NIX_STYLE_TOML};

fn arg_strs(command: &fleet::PlannedCommand) -> Vec<&str> {
    command.args.iter().map(String::as_str).collect()
}

#[test]
fn copy_push_resolves_canonical_host_and_home_shorthand() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_copy(&config, "report.md", "workbox").unwrap();
    assert_eq!(planned.program, "scp");
    assert_eq!(arg_strs(&planned), ["--", "report.md", "workbox:"]);
}

#[test]
fn copy_push_resolves_alias_specific_ssh_target() {
    let config = parse_config(NIX_STYLE_TOML).unwrap();
    let planned = plan_copy(&config, "report.md", "dev:/tmp/report.md").unwrap();
    assert_eq!(
        arg_strs(&planned),
        ["--", "report.md", "dev:/tmp/report.md"]
    );
}

#[test]
fn copy_pull_resolves_remote_source() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_copy(&config, "workbox:/tmp/report.md", ".").unwrap();
    assert_eq!(arg_strs(&planned), ["--", "workbox:/tmp/report.md", "."]);
}

#[test]
fn copy_current_host_endpoint_becomes_local_path() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_copy(&config, "laptop:/tmp/report.md", "workbox:/tmp/report.md").unwrap();
    assert_eq!(
        arg_strs(&planned),
        ["--", "/tmp/report.md", "workbox:/tmp/report.md"]
    );
}

#[test]
fn copy_local_colon_path_with_explicit_prefix_stays_local() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    let planned = plan_copy(&config, "./report:final.md", "workbox").unwrap();
    assert_eq!(arg_strs(&planned), ["--", "./report:final.md", "workbox:"]);
}

#[test]
fn copy_requires_declared_remote_host() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    match plan_copy(&config, "report.md", "ghost:/tmp/report.md") {
        Err(CopyError::UnknownRemoteHost(host)) => assert_eq!(host, "ghost"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn copy_rejects_local_to_local_and_remote_to_remote() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    assert!(matches!(
        plan_copy(&config, "one.md", "two.md"),
        Err(CopyError::RequiresLocalAndRemote)
    ));
    assert!(matches!(
        plan_copy(&config, "workbox:/tmp/one.md", "dev:/tmp/two.md"),
        Err(CopyError::RequiresLocalAndRemote)
    ));
}

#[test]
fn copy_remote_source_requires_a_path() {
    let config = parse_config(MINIMAL_TOML).unwrap();
    assert!(matches!(
        plan_copy(&config, "workbox:", "."),
        Err(CopyError::RemoteSourcePathRequired)
    ));
}

#[test]
fn binary_copy_execs_fake_scp_and_preserves_exit_status() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(NIX_STYLE_TOML);

    let output = fixture
        .fleet()
        .args([
            "copy",
            "-report with spaces.md",
            "dev:/tmp/report with spaces.md",
        ])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fixture.scp_args(),
        Some(vec![
            "--".into(),
            "-report with spaces.md".into(),
            "dev:/tmp/report with spaces.md".into(),
        ])
    );
    assert!(fixture.ssh_args().is_none());

    let output = fixture
        .fleet()
        .env("FLEET_SCP_EXIT", "23")
        .args(["copy", "workbox:/tmp/missing.md", "."])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23));
}

#[test]
fn copy_help_explains_bare_host_destination_and_examples() {
    let fixture = Fixture::new();
    let output = fixture.fleet().args(["copy", "--help"]).output().unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("bare destination host"));
    assert!(stdout.contains("remote user's home directory"));
    assert!(stdout.contains("fleet copy report.md workbox"));
    assert!(stdout.contains("fleet copy workbox:/tmp/report.md ."));
}

#[test]
fn binary_copy_validation_does_not_spawn_scp() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture
        .fleet()
        .args(["copy", "report.md", "ghost:/tmp/report.md"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("copy remote host is not declared: ghost"));
    assert!(!fixture.scp_log.exists());
}
