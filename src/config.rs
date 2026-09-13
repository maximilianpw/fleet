//! TOML configuration: path precedence, schema, and host resolution.
//!
//! Lookup order is `--config PATH`, then `FLEET_CONFIG`, then
//! `$XDG_CONFIG_HOME/fleet/config.toml`, then `$HOME/.config/fleet/config.toml`.
//! An explicit missing path is an error. Default paths do not merge and do
//! not fall back onto a file that was not selected.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::process::ProcessEnv;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_TMUX_COMMAND: &str = "tmux";
pub const DEFAULT_TMUX_SESSION: &str = "main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOrigin {
    Flag(PathBuf),
    Env(PathBuf),
    Default(PathBuf),
}

impl ConfigOrigin {
    pub fn path(&self) -> &Path {
        match self {
            Self::Flag(path) | Self::Env(path) | Self::Default(path) => path,
        }
    }
}

#[derive(Debug, Error)]
pub enum PortError {
    #[error("fleet: ports must be numeric")]
    NotNumeric,
    #[error("fleet: port out of range: {value} (valid ports are 1-65535)")]
    OutOfRange { value: i64 },
}

impl PortError {
    pub fn exit_code(&self) -> i32 {
        2
    }
}

#[derive(Debug, Error)]
pub enum TargetError {
    #[error("fleet: SSH target is empty")]
    Empty,
    #[error("fleet: SSH target must not start with '-': {target}")]
    OptionLike { target: String },
    #[error("fleet: SSH target contains control or whitespace characters: {target}")]
    Unsafe { target: String },
}

impl TargetError {
    pub fn exit_code(&self) -> i32 {
        2
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("fleet: configuration file not found: {path}")]
    ExplicitConfigMissing { path: PathBuf },
    #[error("fleet: FLEET_CONFIG file not found: {path}")]
    EnvConfigMissing { path: PathBuf },
    #[error(
        "fleet: no configuration file found (looked for {path})\ncreate one or pass --config PATH or set FLEET_CONFIG"
    )]
    DefaultConfigMissing { path: PathBuf },
    #[error("fleet: HOME is not set; cannot locate configuration")]
    HomeNotSet,
    #[error("fleet: failed to read configuration {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("fleet: invalid configuration: {0}")]
    Parse(String),
    #[error("fleet: unsupported schema_version {found}; supported version is 1")]
    UnsupportedSchema { found: u32 },
    #[error("fleet: current_host {name} is not a declared host")]
    UnknownCurrentHost { name: String },
    #[error("fleet: duplicate host alias '{alias}'")]
    DuplicateAlias { alias: String },
    #[error("fleet: host alias '{alias}' collides with a canonical host name")]
    AliasCollidesWithHost { alias: String },
    #[error("fleet: alias_targets key '{alias}' is not declared in aliases for host '{host}'")]
    UndeclaredAliasTarget { host: String, alias: String },
    #[error("fleet: duplicate managed local port {port}")]
    DuplicateLocalPort { port: u16 },
    #[error("fleet: tunnel mapping host {host} is not a declared remote host or alias")]
    UnknownMappingHost { host: String },
    #[error("fleet: tunnel mapping host {host} must not be the current host")]
    LocalMappingHost { host: String },
    #[error("fleet: invalid remote tunnel host: {host}")]
    InvalidRemoteHost { host: String },
    #[error("fleet: invalid tunnel label: {label}")]
    InvalidLabel { label: String },
    #[error("fleet: invalid tmux_session for host '{host}'")]
    InvalidTmuxSession { host: String },
    #[error("fleet: invalid tmux_command for host '{host}'")]
    InvalidTmuxCommand { host: String },
    #[error(transparent)]
    Target(#[from] TargetError),
    #[error(transparent)]
    Port(#[from] PortError),
}

impl ConfigError {
    pub fn exit_code(&self) -> i32 {
        2
    }
}

/// TCP port in 1..=65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port(u16);

impl Port {
    pub fn get(self) -> u16 {
        self.0
    }

    pub fn from_i64(value: i64) -> Result<Self, PortError> {
        if (1..=65535).contains(&value) {
            Ok(Self(value as u16))
        } else {
            Err(PortError::OutOfRange { value })
        }
    }
}

impl<'de> Deserialize<'de> for Port {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i64::deserialize(deserializer)?;
        Self::from_i64(value).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for Port {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Supervisor {
    #[default]
    None,
    Launchd,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasTargets {
    pub ssh_target: Option<String>,
    pub tmux_target: Option<String>,
    pub forward_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub ssh_target: String,
    pub aliases: Vec<String>,
    pub os: String,
    pub role: String,
    pub user: String,
    pub client_enrolled: bool,
    pub gui: bool,
    pub long_running_agents: bool,
    pub display_target: Option<String>,
    pub tmux_target: Option<String>,
    pub forward_target: Option<String>,
    pub tmux_command: Option<String>,
    pub tmux_session: Option<String>,
    pub t3code_port: Option<Port>,
    #[serde(default)]
    pub alias_targets: BTreeMap<String, AliasTargets>,
}

impl HostConfig {
    pub fn display_target(&self) -> &str {
        self.display_target.as_deref().unwrap_or(&self.ssh_target)
    }

    pub fn forward_target(&self) -> &str {
        self.forward_target.as_deref().unwrap_or(&self.ssh_target)
    }

    pub fn tmux_command(&self) -> &str {
        self.tmux_command.as_deref().unwrap_or(DEFAULT_TMUX_COMMAND)
    }

    pub fn tmux_session(&self) -> &str {
        self.tmux_session.as_deref().unwrap_or(DEFAULT_TMUX_SESSION)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelMapping {
    pub host: String,
    pub local_port: Port,
    pub remote_port: Port,
    pub remote_host: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelsConfig {
    #[serde(default)]
    pub supervisor: Supervisor,
    #[serde(default)]
    pub mappings: Vec<TunnelMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetConfig {
    pub schema_version: u32,
    pub current_host: String,
    pub hosts: BTreeMap<String, HostConfig>,
    #[serde(default)]
    pub tunnels: TunnelsConfig,
}

/// Connection targets after alias_targets overrides and field defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    pub canonical: String,
    pub token: String,
    pub is_local: bool,
    pub ssh_target: String,
    pub display_target: String,
    pub tmux_target: Option<String>,
    pub forward_target: String,
    pub tmux_command: String,
    pub tmux_session: String,
    pub t3code_port: Option<u16>,
    pub user: String,
    pub role: String,
    pub os: String,
    pub client_enrolled: bool,
    pub aliases: Vec<String>,
}

impl FleetConfig {
    pub fn resolve(&self, token: &str) -> Option<ResolvedHost> {
        if let Some(host) = self.hosts.get(token) {
            return Some(resolve_canonical(token, token, host, self));
        }
        for (canonical, host) in &self.hosts {
            if host.aliases.iter().any(|alias| alias == token) {
                return Some(resolve_alias(canonical, token, host, self));
            }
        }
        None
    }

    pub fn is_local_token(&self, token: &str) -> bool {
        self.resolve(token).is_some_and(|host| host.is_local)
    }

    pub fn mapped_local_ports(&self) -> BTreeSet<u16> {
        self.tunnels
            .mappings
            .iter()
            .map(|mapping| mapping.local_port.get())
            .collect()
    }

    pub fn render_list(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Current machine: {}\n", self.current_host);
        let _ = writeln!(
            out,
            "{:<18} {:<12} {:<24} {:<16} {:<8} ALIASES",
            "HOST", "USER", "TARGET", "ROLE", "CLIENT"
        );
        for (name, host) in &self.hosts {
            let client = if host.client_enrolled { "yes" } else { "no" };
            let _ = writeln!(
                out,
                "{:<18} {:<12} {:<24} {:<16} {:<8} {}",
                name,
                host.user,
                host.display_target(),
                host.role,
                client,
                host.aliases.join(",")
            );
        }
        out
    }
}

fn resolve_canonical(
    canonical: &str,
    token: &str,
    host: &HostConfig,
    config: &FleetConfig,
) -> ResolvedHost {
    ResolvedHost {
        canonical: canonical.to_string(),
        token: token.to_string(),
        is_local: canonical == config.current_host,
        ssh_target: host.ssh_target.clone(),
        display_target: host.display_target().to_string(),
        tmux_target: host.tmux_target.clone(),
        forward_target: host.forward_target().to_string(),
        tmux_command: host.tmux_command().to_string(),
        tmux_session: host.tmux_session().to_string(),
        t3code_port: host.t3code_port.map(Port::get),
        user: host.user.clone(),
        role: host.role.clone(),
        os: host.os.clone(),
        client_enrolled: host.client_enrolled,
        aliases: host.aliases.clone(),
    }
}

fn resolve_alias(
    canonical: &str,
    token: &str,
    host: &HostConfig,
    config: &FleetConfig,
) -> ResolvedHost {
    let overrides = host.alias_targets.get(token);
    let ssh_target = overrides
        .and_then(|value| value.ssh_target.clone())
        .unwrap_or_else(|| host.ssh_target.clone());
    let tmux_target = overrides
        .and_then(|value| value.tmux_target.clone())
        .or_else(|| host.tmux_target.clone());
    let forward_target = overrides
        .and_then(|value| value.forward_target.clone())
        .unwrap_or_else(|| host.forward_target().to_string());
    ResolvedHost {
        canonical: canonical.to_string(),
        token: token.to_string(),
        is_local: canonical == config.current_host,
        ssh_target,
        display_target: host.display_target().to_string(),
        tmux_target,
        forward_target,
        tmux_command: host.tmux_command().to_string(),
        tmux_session: host.tmux_session().to_string(),
        t3code_port: host.t3code_port.map(Port::get),
        user: host.user.clone(),
        role: host.role.clone(),
        os: host.os.clone(),
        client_enrolled: host.client_enrolled,
        aliases: host.aliases.clone(),
    }
}

pub fn parse_port(raw: &str) -> Result<u16, PortError> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PortError::NotNumeric);
    }
    let value: i64 = raw.parse().map_err(|_| PortError::NotNumeric)?;
    Port::from_i64(value).map(Port::get)
}

pub fn is_safe_session_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn is_safe_tmux_command(command: &str) -> bool {
    !command.is_empty()
        && !command.starts_with('-')
        && command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
}

pub fn validate_ssh_target(target: &str) -> Result<(), TargetError> {
    if target.is_empty() {
        return Err(TargetError::Empty);
    }
    if target.starts_with('-') {
        return Err(TargetError::OptionLike {
            target: target.to_string(),
        });
    }
    if target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(TargetError::Unsafe {
            target: target.to_string(),
        });
    }
    Ok(())
}

pub fn is_managed_remote_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn is_safe_label(label: &str) -> bool {
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn resolve_config_origin(
    cli_config: Option<&Path>,
    env: &ProcessEnv,
) -> Result<ConfigOrigin, ConfigError> {
    if let Some(path) = cli_config {
        return Ok(ConfigOrigin::Flag(path.to_path_buf()));
    }
    if let Some(path) = &env.fleet_config {
        return Ok(ConfigOrigin::Env(path.clone()));
    }
    Ok(ConfigOrigin::Default(default_config_path(env)?))
}

pub fn default_config_path(env: &ProcessEnv) -> Result<PathBuf, ConfigError> {
    if let Some(xdg) = &env.xdg_config_home {
        return Ok(xdg.join("fleet/config.toml"));
    }
    match &env.home {
        Some(home) => Ok(home.join(".config/fleet/config.toml")),
        None => Err(ConfigError::HomeNotSet),
    }
}

pub fn load_config(
    cli_config: Option<&Path>,
    env: &ProcessEnv,
) -> Result<FleetConfig, ConfigError> {
    let origin = resolve_config_origin(cli_config, env)?;
    let path = origin.path();
    if !path.is_file() {
        return Err(match &origin {
            ConfigOrigin::Flag(path) => ConfigError::ExplicitConfigMissing { path: path.clone() },
            ConfigOrigin::Env(path) => ConfigError::EnvConfigMissing { path: path.clone() },
            ConfigOrigin::Default(path) => ConfigError::DefaultConfigMissing { path: path.clone() },
        });
    }
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_config(&text)
}

pub fn parse_config(text: &str) -> Result<FleetConfig, ConfigError> {
    let config: FleetConfig =
        toml::from_str(text).map_err(|error| ConfigError::Parse(error.to_string()))?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &FleetConfig) -> Result<(), ConfigError> {
    if config.schema_version != SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchema {
            found: config.schema_version,
        });
    }
    if !config.hosts.contains_key(&config.current_host) {
        return Err(ConfigError::UnknownCurrentHost {
            name: config.current_host.clone(),
        });
    }

    let canonicals: BTreeSet<&str> = config.hosts.keys().map(String::as_str).collect();
    let mut aliases_seen: BTreeMap<&str, &str> = BTreeMap::new();

    for (name, host) in &config.hosts {
        validate_ssh_target(name)?;
        validate_ssh_target(&host.ssh_target)?;
        if let Some(target) = &host.display_target {
            if target.chars().any(char::is_control) {
                return Err(TargetError::Unsafe {
                    target: target.clone(),
                }
                .into());
            }
        }
        if let Some(target) = &host.tmux_target {
            validate_ssh_target(target)?;
        }
        if let Some(target) = &host.forward_target {
            validate_ssh_target(target)?;
        }
        if let Some(command) = &host.tmux_command {
            if !is_safe_tmux_command(command) {
                return Err(ConfigError::InvalidTmuxCommand { host: name.clone() });
            }
        }
        if let Some(session) = &host.tmux_session {
            if !is_safe_session_name(session) {
                return Err(ConfigError::InvalidTmuxSession { host: name.clone() });
            }
        }

        for alias in &host.aliases {
            validate_ssh_target(alias)?;
            if canonicals.contains(alias.as_str()) {
                return Err(ConfigError::AliasCollidesWithHost {
                    alias: alias.clone(),
                });
            }
            if aliases_seen.insert(alias, name).is_some() {
                return Err(ConfigError::DuplicateAlias {
                    alias: alias.clone(),
                });
            }
        }

        for key in host.alias_targets.keys() {
            if !host.aliases.iter().any(|alias| alias == key) {
                return Err(ConfigError::UndeclaredAliasTarget {
                    host: name.clone(),
                    alias: key.clone(),
                });
            }
        }
        for targets in host.alias_targets.values() {
            if let Some(target) = &targets.ssh_target {
                validate_ssh_target(target)?;
            }
            if let Some(target) = &targets.tmux_target {
                validate_ssh_target(target)?;
            }
            if let Some(target) = &targets.forward_target {
                validate_ssh_target(target)?;
            }
        }
    }

    let mut local_ports: BTreeSet<u16> = BTreeSet::new();
    for mapping in &config.tunnels.mappings {
        let port = mapping.local_port.get();
        if !local_ports.insert(port) {
            return Err(ConfigError::DuplicateLocalPort { port });
        }
        if !is_managed_remote_host(&mapping.remote_host) {
            return Err(ConfigError::InvalidRemoteHost {
                host: mapping.remote_host.clone(),
            });
        }
        if !is_safe_label(&mapping.label) {
            return Err(ConfigError::InvalidLabel {
                label: mapping.label.clone(),
            });
        }
        let Some(resolved) = config.resolve(&mapping.host) else {
            return Err(ConfigError::UnknownMappingHost {
                host: mapping.host.clone(),
            });
        };
        if resolved.is_local {
            return Err(ConfigError::LocalMappingHost {
                host: mapping.host.clone(),
            });
        }
    }

    Ok(())
}
