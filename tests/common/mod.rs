#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub const MINIMAL_TOML: &str = r#"
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
tmux_command = "tmux"
tmux_session = "main"
"#;

pub const NIX_STYLE_TOML: &str = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
display_target = "laptop.example.test"
aliases = ["portable"]
os = "darwin"
role = "interface"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = false

[hosts.workbox]
ssh_target = "workbox"
display_target = "workbox.example.test"
tmux_target = "tm-workbox"
forward_target = "fleet-forward-workbox"
aliases = ["dev"]
os = "nixos"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = true
tmux_command = "/run/current-system/sw/bin/tmux"
tmux_session = "main"
t3code_port = 51001

[hosts.workbox.alias_targets.dev]
ssh_target = "dev"
tmux_target = "tm-dev"
forward_target = "fleet-forward-dev"

[tunnels]
supervisor = "launchd"

[[tunnels.mappings]]
host = "workbox"
local_port = 5173
remote_port = 5173
remote_host = "localhost"
label = "org.nix-community.home.fleet-tunnel-5173"
"#;

pub struct Fixture {
    pub root: PathBuf,
    pub home: PathBuf,
    pub xdg: PathBuf,
    pub bin: PathBuf,
    pub ssh_log: PathBuf,
    pub tmux_log: PathBuf,
    pub shell_log: PathBuf,
    pub dummy_log: PathBuf,
    pub kill_log: PathBuf,
    pub ps_output: PathBuf,
    pub ssh_meta: PathBuf,
    pub ssh_ready: PathBuf,
    pub ssh_signal: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let id = format!(
            "{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            nanos
        );
        let root = std::env::temp_dir().join("fleet-tests").join(id);
        let home = root.join("home");
        let xdg = root.join("xdg");
        let bin = root.join("bin");
        fs::create_dir_all(home.join(".config/fleet")).expect("home config dir");
        fs::create_dir_all(xdg.join("fleet")).expect("xdg config dir");
        fs::create_dir_all(&bin).expect("bin dir");

        let fixture = Self {
            ssh_log: root.join("ssh-args"),
            tmux_log: root.join("tmux-args"),
            shell_log: root.join("shell-args"),
            dummy_log: root.join("dummy-args"),
            kill_log: root.join("kill-args"),
            ps_output: root.join("ps-output"),
            ssh_meta: root.join("ssh-meta"),
            ssh_ready: root.join("ssh-ready"),
            ssh_signal: root.join("ssh-signal"),
            root,
            home,
            xdg,
            bin,
        };
        fs::write(&fixture.ps_output, "").expect("ps output");
        fixture.write_fakes();
        fixture
    }

    pub fn write_xdg_config(&self, toml: &str) {
        fs::write(self.xdg.join("fleet/config.toml"), toml).expect("write xdg config");
    }

    pub fn write_home_config(&self, toml: &str) {
        fs::write(self.home.join(".config/fleet/config.toml"), toml).expect("write home config");
    }

    pub fn write_config_at(&self, path: &Path, toml: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("config parent");
        }
        fs::write(path, toml).expect("write config path");
    }

    pub fn set_ps(&self, body: &str) {
        fs::write(&self.ps_output, body).expect("write ps output");
    }

    pub fn fleet(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleet"));
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg)
            .env("PATH", self.path())
            .env("SHELL", self.bin.join("fakeshell"))
            .env("FLEET_SSH_ARGS_LOG", &self.ssh_log)
            .env("FLEET_TMUX_ARGS_LOG", &self.tmux_log)
            .env("FLEET_SHELL_ARGS_LOG", &self.shell_log)
            .env("FLEET_DUMMY_ARGS_LOG", &self.dummy_log)
            .env("FLEET_KILL_LOG", &self.kill_log)
            .env("FLEET_PS_OUTPUT", &self.ps_output)
            .env("FLEET_SSH_META_LOG", &self.ssh_meta)
            .env("FLEET_SSH_READY", &self.ssh_ready)
            .env("FLEET_SSH_SIGNAL_LOG", &self.ssh_signal)
            .env("LC_ALL", "C")
            .current_dir(&self.root);
        cmd
    }

    pub fn path(&self) -> String {
        let rest = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        format!("{}:{rest}", self.bin.display())
    }

    pub fn ssh_args(&self) -> Option<Vec<String>> {
        read_lines(&self.ssh_log)
    }

    pub fn tmux_args(&self) -> Option<Vec<String>> {
        read_lines(&self.tmux_log)
    }

    pub fn shell_args(&self) -> Option<Vec<String>> {
        read_lines(&self.shell_log)
    }

    pub fn dummy_args(&self) -> Option<Vec<String>> {
        read_lines(&self.dummy_log)
    }

    pub fn kill_args(&self) -> Option<Vec<String>> {
        read_lines(&self.kill_log)
    }

    pub fn output_text(output: &Output) -> (String, String, i32) {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let code = output.status.code().unwrap_or(255);
        (stdout, stderr, code)
    }

    fn write_fakes(&self) {
        write_script(
            &self.bin.join("ssh"),
            r#"#!/bin/sh
log=${FLEET_SSH_ARGS_LOG:?}
: > "$log"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$log"
done
if [ -n "${FLEET_SSH_META_LOG:-}" ]; then
  if [ -t 0 ]; then
    printf 'stdin_tty=yes\n' > "$FLEET_SSH_META_LOG"
  else
    printf 'stdin_tty=no\n' > "$FLEET_SSH_META_LOG"
  fi
fi
if [ -n "${FLEET_SSH_HOLD:-}" ]; then
  trap 'printf TERM > "$FLEET_SSH_SIGNAL_LOG"; exit 0' TERM INT
  printf ready > "$FLEET_SSH_READY"
  while true; do
    sleep 1
  done
fi
exit 0
"#,
        );
        write_script(
            &self.bin.join("tmux"),
            r#"#!/bin/sh
log=${FLEET_TMUX_ARGS_LOG:?}
: > "$log"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$log"
done
exit 0
"#,
        );
        write_script(
            &self.bin.join("ps"),
            r#"#!/bin/sh
if [ -n "${FLEET_PS_OUTPUT:-}" ] && [ -f "$FLEET_PS_OUTPUT" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    printf '%s\n' "$line"
  done < "$FLEET_PS_OUTPUT"
  exit 0
fi
exit 1
"#,
        );
        write_script(
            &self.bin.join("kill"),
            r#"#!/bin/sh
printf '%s\n' "$@" >> "${FLEET_KILL_LOG:?}"
exit 0
"#,
        );
        write_script(
            &self.bin.join("fakeshell"),
            r#"#!/bin/sh
log=${FLEET_SHELL_ARGS_LOG:?}
: > "$log"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$log"
done
printf 'program=%s\n' "$0" >> "$log"
exit 0
"#,
        );
        write_script(
            &self.bin.join("dummycmd"),
            r#"#!/bin/sh
log=${FLEET_DUMMY_ARGS_LOG:?}
: > "$log"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$log"
done
exit 0
"#,
        );
    }
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
}

fn read_lines(path: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().map(str::to_string).collect())
}
