mod common;

use std::path::PathBuf;

use fleet::config::{
    load_config, parse_config, resolve_config_origin, ConfigError, ConfigOrigin, Supervisor,
    SCHEMA_VERSION,
};
use fleet::{doctor_mappings_from_config, parse_port, PortError, ProcessEnv};

use common::{Fixture, MINIMAL_TOML, NIX_STYLE_TOML};

#[test]
fn parse_minimal_example() {
    let config = parse_config(MINIMAL_TOML).expect("minimal config");
    assert_eq!(config.schema_version, SCHEMA_VERSION);
    assert_eq!(config.current_host, "laptop");
    assert_eq!(config.tunnels.supervisor, Supervisor::None);
    assert!(config.tunnels.mappings.is_empty());
    let workbox = config.resolve("workbox").expect("workbox");
    assert_eq!(workbox.ssh_target, "workbox");
    assert_eq!(workbox.forward_target, "workbox");
    assert!(workbox.tmux_target.is_none());
    assert_eq!(workbox.tmux_command, "tmux");
    assert_eq!(workbox.tmux_session, "main");
    assert!(!workbox.is_local);
    let laptop = config.resolve("laptop").expect("laptop");
    assert!(laptop.is_local);
}

#[test]
fn nix_projection_fields_round_trip_through_validation() {
    let config = parse_config(NIX_STYLE_TOML).expect("nix-style config");
    let workbox = config.resolve("workbox").expect("workbox");
    assert_eq!(workbox.display_target, "workbox.example.test");
    assert_eq!(workbox.ssh_target, "workbox");
    assert_eq!(workbox.tmux_target.as_deref(), Some("tm-workbox"));
    assert_eq!(workbox.forward_target, "fleet-forward-workbox");
    assert_eq!(workbox.os, "nixos");
    assert_eq!(workbox.t3code_port, Some(51001));
    assert_eq!(workbox.tmux_command, "/run/current-system/sw/bin/tmux");

    let dev = config.resolve("dev").expect("dev alias");
    assert_eq!(dev.canonical, "workbox");
    assert_eq!(dev.ssh_target, "dev");
    assert_eq!(dev.tmux_target.as_deref(), Some("tm-dev"));
    assert_eq!(dev.forward_target, "fleet-forward-dev");
    assert_eq!(dev.display_target, "workbox.example.test");

    let portable = config.resolve("portable").expect("local alias");
    assert!(portable.is_local);
    assert_eq!(portable.canonical, "laptop");

    assert_eq!(config.tunnels.supervisor, Supervisor::Launchd);
    assert_eq!(config.tunnels.mappings.len(), 1);
    assert_eq!(config.tunnels.mappings[0].host, "workbox");
    assert_eq!(
        config.tunnels.mappings[0].label,
        "org.nix-community.home.fleet-tunnel-5173"
    );
}

#[test]
fn alias_overrides_inherit_unspecified_targets() {
    let toml = r#"
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
tmux_target = "tm-workbox"
forward_target = "fleet-forward-workbox"
aliases = ["dev"]
os = "linux"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = true

[hosts.workbox.alias_targets.dev]
ssh_target = "dev"
"#;
    let config = parse_config(toml).expect("partial alias overrides");
    let dev = config.resolve("dev").expect("dev");
    assert_eq!(dev.ssh_target, "dev");
    assert_eq!(dev.tmux_target.as_deref(), Some("tm-workbox"));
    assert_eq!(dev.forward_target, "fleet-forward-workbox");
}

#[test]
fn list_uses_display_target_and_sorted_canonical_names() {
    let config = parse_config(NIX_STYLE_TOML).expect("nix-style config");
    let list = config.render_list();
    assert!(list.starts_with("Current machine: laptop\n\n"));
    assert!(list.contains("HOST               USER         TARGET                   ROLE             CLIENT   ALIASES\n"));
    let host_lines: Vec<_> = list
        .lines()
        .filter(|line| line.starts_with("laptop") || line.starts_with("workbox"))
        .collect();
    assert_eq!(host_lines.len(), 2);
    assert!(host_lines[0].starts_with("laptop"));
    assert!(host_lines[1].starts_with("workbox"));
    assert!(host_lines[0].contains("laptop.example.test"));
    assert!(host_lines[1].contains("workbox.example.test"));
    assert!(host_lines[0].contains("yes"));
    assert!(host_lines[1].contains("no") || host_lines[1].contains("yes"));
    assert!(host_lines[0].contains("portable"));
    assert!(host_lines[1].contains("dev"));
    assert!(!list.contains("tm-workbox"));
}

#[test]
fn list_golden_minimal() {
    let config = parse_config(MINIMAL_TOML).expect("minimal");
    let expected = "Current machine: laptop\n\nHOST               USER         TARGET                   ROLE             CLIENT   ALIASES\nlaptop             developer    laptop                   interface        yes      \nworkbox            developer    workbox                  compute          no       dev\n";
    assert_eq!(config.render_list(), expected);
}

#[test]
fn unknown_field_is_rejected() {
    let error = parse_config(&format!("{MINIMAL_TOML}\nextra = 1\n")).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("unknown field"), "{message}");
}

#[test]
fn camel_case_inventory_field_is_unknown() {
    let toml = r#"
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
hostName = "laptop.example.test"
"#;
    let error = parse_config(toml).unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn unsupported_schema_version_is_rejected() {
    let toml = MINIMAL_TOML.replace("schema_version = 1", "schema_version = 2");
    let error = parse_config(&toml).unwrap_err();
    assert!(error.to_string().contains("unsupported schema_version 2"));
}

#[test]
fn missing_required_host_field_is_rejected() {
    let toml = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
aliases = []
os = "darwin"
role = "interface"
user = "developer"
gui = true
long_running_agents = false
"#;
    let error = parse_config(toml).unwrap_err();
    assert!(error
        .to_string()
        .contains("missing field `client_enrolled`"));
}

#[test]
fn aliases_are_required() {
    let toml = MINIMAL_TOML.replacen("aliases = []\n", "", 1);
    let error = parse_config(&toml).unwrap_err();
    assert!(error.to_string().contains("missing field `aliases`"));
}

#[test]
fn unsafe_tmux_command_is_rejected() {
    let toml = MINIMAL_TOML.replace(
        "tmux_command = \"tmux\"",
        "tmux_command = \"tmux; touch /tmp/fleet\"",
    );
    let error = parse_config(&toml).unwrap_err();
    assert!(error.to_string().contains("invalid tmux_command"));
}

#[test]
fn unknown_current_host_is_rejected() {
    let toml = MINIMAL_TOML.replace("current_host = \"laptop\"", "current_host = \"missing\"");
    let error = parse_config(&toml).unwrap_err();
    match error {
        ConfigError::UnknownCurrentHost { name } => assert_eq!(name, "missing"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn duplicate_alias_is_rejected() {
    let toml = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
aliases = ["dev"]
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
client_enrolled = false
gui = false
long_running_agents = true
"#;
    let error = parse_config(toml).unwrap_err();
    match error {
        ConfigError::DuplicateAlias { alias } => assert_eq!(alias, "dev"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn alias_colliding_with_canonical_name_is_rejected() {
    let toml = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
aliases = ["workbox"]
os = "darwin"
role = "interface"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = false

[hosts.workbox]
ssh_target = "workbox"
aliases = []
os = "linux"
role = "compute"
user = "developer"
client_enrolled = false
gui = false
long_running_agents = true
"#;
    let error = parse_config(toml).unwrap_err();
    match error {
        ConfigError::AliasCollidesWithHost { alias } => assert_eq!(alias, "workbox"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn alias_targets_key_must_be_declared_alias() {
    let toml = r#"
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
client_enrolled = false
gui = false
long_running_agents = true

[hosts.workbox.alias_targets.other]
ssh_target = "other"
"#;
    let error = parse_config(toml).unwrap_err();
    match error {
        ConfigError::UndeclaredAliasTarget { host, alias } => {
            assert_eq!(host, "workbox");
            assert_eq!(alias, "other");
        }
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn option_like_ssh_target_is_rejected() {
    let toml = MINIMAL_TOML.replace(
        "ssh_target = \"workbox\"",
        "ssh_target = \"-oBatchMode=yes\"",
    );
    let error = parse_config(&toml).unwrap_err();
    assert!(error.to_string().contains("must not start with '-'"));
}

#[test]
fn control_character_in_ssh_target_is_rejected() {
    let toml = MINIMAL_TOML.replace("ssh_target = \"workbox\"", "ssh_target = \"work\\nbox\"");
    let error = parse_config(&toml).unwrap_err();
    assert!(
        error.to_string().contains("control or whitespace"),
        "{error}"
    );
}

#[test]
fn mapping_to_current_host_is_rejected() {
    let toml = format!(
        "{MINIMAL_TOML}
[tunnels]
supervisor = \"none\"

[[tunnels.mappings]]
host = \"laptop\"
local_port = 5173
remote_port = 5173
remote_host = \"localhost\"
label = \"org.nix-community.home.fleet-tunnel-5173\"
"
    );
    let error = parse_config(&toml).unwrap_err();
    match error {
        ConfigError::LocalMappingHost { host } => assert_eq!(host, "laptop"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn mapping_to_unknown_host_is_rejected() {
    let toml = format!(
        "{MINIMAL_TOML}
[[tunnels.mappings]]
host = \"ghost\"
local_port = 5173
remote_port = 5173
remote_host = \"localhost\"
label = \"org.nix-community.home.fleet-tunnel-5173\"
"
    );
    let error = parse_config(&toml).unwrap_err();
    match error {
        ConfigError::UnknownMappingHost { host } => assert_eq!(host, "ghost"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn mapping_alias_of_current_host_is_rejected() {
    let toml = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
aliases = ["portable"]
os = "darwin"
role = "interface"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = false

[hosts.workbox]
ssh_target = "workbox"
aliases = []
os = "linux"
role = "compute"
user = "developer"
client_enrolled = false
gui = false
long_running_agents = true

[[tunnels.mappings]]
host = "portable"
local_port = 5173
remote_port = 5173
remote_host = "localhost"
label = "org.nix-community.home.fleet-tunnel-5173"
"#;
    let error = parse_config(toml).unwrap_err();
    match error {
        ConfigError::LocalMappingHost { host } => assert_eq!(host, "portable"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn mapping_to_remote_alias_is_accepted() {
    let toml = format!(
        "{MINIMAL_TOML}
[[tunnels.mappings]]
host = \"dev\"
local_port = 5173
remote_port = 5173
remote_host = \"localhost\"
label = \"org.nix-community.home.fleet-tunnel-5173\"
"
    );
    parse_config(&toml).expect("alias mapping");
}

#[test]
fn doctor_canonicalizes_mapping_hosts_declared_as_aliases() {
    let toml = format!(
        "{MINIMAL_TOML}\n[[tunnels.mappings]]\nhost = \"dev\"\nlocal_port = 5173\nremote_port = 5173\nremote_host = \"localhost\"\nlabel = \"org.nix-community.home.fleet-tunnel-5173\"\n"
    );
    let config = parse_config(&toml).expect("alias mapping");
    let mappings = doctor_mappings_from_config(&config);
    assert_eq!(mappings[0].host, "workbox");
}

#[test]
fn duplicate_local_ports_are_rejected() {
    let toml = format!(
        "{MINIMAL_TOML}
[[tunnels.mappings]]
host = \"workbox\"
local_port = 5173
remote_port = 5173
remote_host = \"localhost\"
label = \"org.nix-community.home.fleet-tunnel-5173\"

[[tunnels.mappings]]
host = \"workbox\"
local_port = 5173
remote_port = 8080
remote_host = \"localhost\"
label = \"org.nix-community.home.fleet-tunnel-8080\"
"
    );
    let error = parse_config(&toml).unwrap_err();
    match error {
        ConfigError::DuplicateLocalPort { port } => assert_eq!(port, 5173),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn port_zero_is_rejected() {
    let toml = format!(
        "{MINIMAL_TOML}
[[tunnels.mappings]]
host = \"workbox\"
local_port = 0
remote_port = 5173
remote_host = \"localhost\"
label = \"org.nix-community.home.fleet-tunnel-0\"
"
    );
    let error = parse_config(&toml).unwrap_err();
    assert!(error.to_string().contains("port out of range"), "{error}");
}

#[test]
fn ipv6_managed_remote_host_is_rejected() {
    let toml = format!(
        "{MINIMAL_TOML}
[[tunnels.mappings]]
host = \"workbox\"
local_port = 5173
remote_port = 5173
remote_host = \"::1\"
label = \"org.nix-community.home.fleet-tunnel-5173\"
"
    );
    let error = parse_config(&toml).unwrap_err();
    match error {
        ConfigError::InvalidRemoteHost { host } => assert_eq!(host, "::1"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn invalid_label_is_rejected() {
    let toml = format!(
        "{MINIMAL_TOML}
[[tunnels.mappings]]
host = \"workbox\"
local_port = 5173
remote_port = 5173
remote_host = \"localhost\"
label = \"../not a label\"
"
    );
    let error = parse_config(&toml).unwrap_err();
    match error {
        ConfigError::InvalidLabel { label } => assert_eq!(label, "../not a label"),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn unknown_os_string_is_allowed() {
    let toml = MINIMAL_TOML.replace("os = \"linux\"", "os = \"plan9\"");
    let config = parse_config(&toml).expect("unknown os");
    assert_eq!(config.hosts["workbox"].os, "plan9");
}

#[test]
fn parse_port_rejects_non_numeric_and_out_of_range() {
    match parse_port("abc") {
        Err(PortError::NotNumeric) => {}
        other => panic!("unexpected {other:?}"),
    }
    match parse_port("0") {
        Err(PortError::OutOfRange { value }) => assert_eq!(value, 0),
        other => panic!("unexpected {other:?}"),
    }
    match parse_port("65536") {
        Err(PortError::OutOfRange { value }) => assert_eq!(value, 65536),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(parse_port("65535").unwrap(), 65535);
}

#[test]
fn cli_config_wins_over_env_and_xdg() {
    let fixture = Fixture::new();
    let flag = fixture.root.join("flag.toml");
    let env_path = fixture.root.join("env.toml");
    let flag_toml = MINIMAL_TOML.replace("current_host = \"laptop\"", "current_host = \"workbox\"");
    let env_toml = MINIMAL_TOML.replace("role = \"compute\"", "role = \"env-role\"");
    let xdg_toml = MINIMAL_TOML.replace("role = \"interface\"", "role = \"xdg-role\"");
    fixture.write_config_at(&flag, &flag_toml);
    fixture.write_config_at(&env_path, &env_toml);
    fixture.write_xdg_config(&xdg_toml);

    let env = ProcessEnv {
        home: Some(fixture.home.clone()),
        xdg_config_home: Some(fixture.xdg.clone()),
        fleet_config: Some(env_path.clone()),
        shell: None,
        path: None,
    };
    let loaded = load_config(Some(&flag), &env).expect("flag config");
    assert_eq!(loaded.current_host, "workbox");
}

#[test]
fn env_config_wins_over_xdg() {
    let fixture = Fixture::new();
    let env_path = fixture.root.join("env.toml");
    let env_toml = MINIMAL_TOML.replace("current_host = \"laptop\"", "current_host = \"workbox\"");
    fixture.write_config_at(&env_path, &env_toml);
    fixture.write_xdg_config(MINIMAL_TOML);
    let env = ProcessEnv {
        home: Some(fixture.home.clone()),
        xdg_config_home: Some(fixture.xdg.clone()),
        fleet_config: Some(env_path),
        shell: None,
        path: None,
    };
    let loaded = load_config(None, &env).expect("env config");
    assert_eq!(loaded.current_host, "workbox");
}

#[test]
fn xdg_wins_over_home_default() {
    let fixture = Fixture::new();
    let xdg_toml = MINIMAL_TOML.replace("current_host = \"laptop\"", "current_host = \"workbox\"");
    fixture.write_xdg_config(&xdg_toml);
    fixture.write_home_config(MINIMAL_TOML);
    let env = ProcessEnv {
        home: Some(fixture.home.clone()),
        xdg_config_home: Some(fixture.xdg.clone()),
        fleet_config: None,
        shell: None,
        path: None,
    };
    let loaded = load_config(None, &env).expect("xdg config");
    assert_eq!(loaded.current_host, "workbox");
}

#[test]
fn home_default_used_when_xdg_unset() {
    let fixture = Fixture::new();
    fixture.write_home_config(MINIMAL_TOML);
    let env = ProcessEnv {
        home: Some(fixture.home.clone()),
        xdg_config_home: None,
        fleet_config: None,
        shell: None,
        path: None,
    };
    let origin = resolve_config_origin(None, &env).expect("origin");
    match origin {
        ConfigOrigin::Default(path) => {
            assert_eq!(path, fixture.home.join(".config/fleet/config.toml"));
        }
        other => panic!("unexpected {other:?}"),
    }
    let loaded = load_config(None, &env).expect("home config");
    assert_eq!(loaded.current_host, "laptop");
}

#[test]
fn missing_explicit_config_does_not_fall_back() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let missing = fixture.root.join("missing.toml");
    let env = ProcessEnv {
        home: Some(fixture.home.clone()),
        xdg_config_home: Some(fixture.xdg.clone()),
        fleet_config: None,
        shell: None,
        path: None,
    };
    let error = load_config(Some(&missing), &env).unwrap_err();
    match error {
        ConfigError::ExplicitConfigMissing { path } => assert_eq!(path, missing),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn missing_env_config_does_not_fall_back() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let missing = fixture.root.join("missing-env.toml");
    let env = ProcessEnv {
        home: Some(fixture.home.clone()),
        xdg_config_home: Some(fixture.xdg.clone()),
        fleet_config: Some(missing.clone()),
        shell: None,
        path: None,
    };
    let error = load_config(None, &env).unwrap_err();
    match error {
        ConfigError::EnvConfigMissing { path } => assert_eq!(path, missing),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn missing_default_config_is_a_setup_error() {
    let env = ProcessEnv {
        home: Some(PathBuf::from("/nonexistent/fleet-home")),
        xdg_config_home: Some(PathBuf::from("/nonexistent/fleet-xdg")),
        fleet_config: None,
        shell: None,
        path: None,
    };
    let error = load_config(None, &env).unwrap_err();
    match error {
        ConfigError::DefaultConfigMissing { ref path } => {
            assert_eq!(
                path,
                &PathBuf::from("/nonexistent/fleet-xdg/fleet/config.toml")
            );
        }
        other => panic!("unexpected {other}"),
    }
    assert!(error
        .to_string()
        .contains("create one or pass --config PATH"));
}

#[test]
fn help_and_version_work_without_config() {
    let fixture = Fixture::new();
    let mut help = fixture.fleet();
    help.env_remove("XDG_CONFIG_HOME");
    help.env("HOME", "/nonexistent/fleet-empty-home");
    let output = help.arg("--help").output().expect("help");
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("usage:"));
    assert!(stdout.contains("fleet config validate"));
    assert!(stdout.contains("fleet completions SHELL"));
    assert!(!fixture.ssh_log.exists());

    let mut version = fixture.fleet();
    version.env_remove("XDG_CONFIG_HOME");
    version.env("HOME", "/nonexistent/fleet-empty-home");
    let output = version.arg("--version").output().expect("version");
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("fleet"));
    assert!(!fixture.ssh_log.exists());
}

#[test]
fn completions_work_without_config() {
    let fixture = Fixture::new();
    let mut cmd = fixture.fleet();
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env("HOME", "/nonexistent/fleet-empty-home");
    let output = cmd
        .args(["completions", "bash"])
        .output()
        .expect("completions");
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("_fleet") || stdout.contains("fleet"));
    assert!(!fixture.ssh_log.exists());
}

#[test]
fn config_validate_success_and_invalid_exit_codes() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture
        .fleet()
        .args(["config", "validate"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(!fixture.ssh_log.exists());

    fixture.write_xdg_config("schema_version = 2\n");
    let output = fixture
        .fleet()
        .args(["config", "validate"])
        .output()
        .unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("invalid configuration") || stderr.contains("unsupported schema_version")
    );
    assert!(!fixture.ssh_log.exists());
}

#[test]
fn no_arguments_runs_list() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let bare = fixture.fleet().output().unwrap();
    let listed = fixture.fleet().arg("list").output().unwrap();
    let (bare_out, bare_err, bare_code) = Fixture::output_text(&bare);
    let (list_out, list_err, list_code) = Fixture::output_text(&listed);
    assert_eq!(bare_code, 0, "{bare_err}");
    assert_eq!(list_code, 0, "{list_err}");
    assert_eq!(bare_out, list_out);
    assert!(bare_out.starts_with("Current machine: laptop\n\n"));
}

#[test]
fn current_host_is_not_inferred_from_hostname() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let output = fixture
        .fleet()
        .env("HOSTNAME", "workbox")
        .arg("list")
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("Current machine: laptop\n\n"));
}

#[test]
fn invalid_config_spawns_no_ssh() {
    let fixture = Fixture::new();
    fixture.write_xdg_config("not toml");
    let output = fixture.fleet().args(["ssh", "workbox"]).output().unwrap();
    let (_, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 2, "{stderr}");
    assert!(!fixture.ssh_log.exists());
    assert!(!fixture.tmux_log.exists());
}

#[test]
fn config_flag_is_used_by_the_binary() {
    let fixture = Fixture::new();
    fixture.write_xdg_config(MINIMAL_TOML);
    let flag = fixture.root.join("other.toml");
    fixture.write_config_at(
        &flag,
        &MINIMAL_TOML.replace("current_host = \"laptop\"", "current_host = \"workbox\""),
    );
    let output = fixture
        .fleet()
        .args(["--config", flag.to_str().unwrap(), "list"])
        .output()
        .unwrap();
    let (stdout, stderr, code) = Fixture::output_text(&output);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.starts_with("Current machine: workbox\n\n"));
}
