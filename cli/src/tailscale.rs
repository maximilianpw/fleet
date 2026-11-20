//! Host presence from the local `tailscale status --json`.
//!
//! Presence is advisory. A missing or failing Tailscale CLI yields
//! [`Presence::Unknown`] rather than an error, and nothing else in Fleet
//! depends on Tailscale.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::FleetConfig;
use crate::process::run_with_deadline;

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const MACOS_APP_CLI: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
/// Tailscale reports this for peers that are online right now.
const ZERO_TIME_PREFIX: &str = "0001-01-01";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Presence {
    /// The current machine.
    #[serde(rename = "self")]
    This,
    Online {
        path: NetPath,
    },
    Offline {
        last_seen: Option<String>,
    },
    /// Tailscale status was read, but no peer matched this host.
    NotInTailnet,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "via", rename_all = "snake_case")]
pub enum NetPath {
    Direct,
    Relay(String),
    /// Online but no recent traffic, so Tailscale has no path to report.
    Idle,
}

impl Presence {
    /// Online state for `--where online`. A host Tailscale does not list is
    /// not online there, though it may still be reachable another way.
    pub fn online(&self) -> Option<bool> {
        match self {
            Self::This | Self::Online { .. } => Some(true),
            Self::Offline { .. } | Self::NotInTailnet => Some(false),
            Self::Unknown => None,
        }
    }

    pub fn network_label(&self) -> String {
        match self {
            Self::This => "self".into(),
            Self::Online { .. } => "online".into(),
            Self::Offline {
                last_seen: Some(seen),
            } => format!("offline (seen {seen})"),
            Self::Offline { last_seen: None } => "offline".into(),
            Self::NotInTailnet => "not-in-tailnet".into(),
            Self::Unknown => "unknown".into(),
        }
    }

    pub fn path_label(&self) -> String {
        match self {
            Self::Online {
                path: NetPath::Direct,
            } => "direct".into(),
            Self::Online {
                path: NetPath::Relay(relay),
            } => format!("relay:{relay}"),
            Self::Online {
                path: NetPath::Idle,
            } => "idle".into(),
            _ => "-".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Peer {
    #[serde(default)]
    pub host_name: String,
    #[serde(default, rename = "DNSName")]
    pub dns_name: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub cur_addr: String,
    #[serde(default)]
    pub relay: String,
    #[serde(default)]
    pub last_seen: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Status {
    #[serde(default)]
    pub backend_state: String,
    #[serde(rename = "Self", default)]
    pub this: Option<Peer>,
    #[serde(default)]
    pub peer: BTreeMap<String, Peer>,
}

pub fn parse_status(json: &str) -> Option<Status> {
    serde_json::from_str(json).ok()
}

impl Status {
    /// False while Tailscale is stopped or logged out. Its peer list then
    /// says every peer is offline, which is not evidence about the peers.
    pub fn backend_running(&self) -> bool {
        self.backend_state.is_empty() || self.backend_state == "Running"
    }
}

/// Read `tailscale status --json` from PATH, or the macOS app bundle CLI.
/// `None` unless Tailscale is running, so presence stays unknown.
pub fn read_status() -> Option<Status> {
    for program in ["tailscale", MACOS_APP_CLI] {
        let mut cmd = Command::new(program);
        cmd.args(["status", "--json"]);
        let Ok(output) = run_with_deadline(
            &mut cmd,
            STATUS_TIMEOUT,
            Duration::from_secs(1),
            Duration::from_millis(20),
        ) else {
            continue;
        };
        // `tailscale status` exits non-zero when logged out but still prints JSON.
        if let Some(status) = parse_status(&String::from_utf8_lossy(&output.stdout)) {
            return status.backend_running().then_some(status);
        }
    }
    None
}

/// Presence for every configured host, keyed by canonical name.
pub fn presence_for(config: &FleetConfig, status: Option<&Status>) -> BTreeMap<String, Presence> {
    config
        .hosts
        .keys()
        .map(|name| {
            let presence = if *name == config.current_host {
                Presence::This
            } else {
                match status {
                    None => Presence::Unknown,
                    Some(status) => match find_peer(config, name, status) {
                        Some(peer) => peer_presence(peer),
                        None => Presence::NotInTailnet,
                    },
                }
            };
            (name.clone(), presence)
        })
        .collect()
}

fn peer_presence(peer: &Peer) -> Presence {
    if !peer.online {
        let last_seen = (!peer.last_seen.is_empty()
            && !peer.last_seen.starts_with(ZERO_TIME_PREFIX))
        .then(|| short_time(&peer.last_seen));
        return Presence::Offline { last_seen };
    }
    let path = if !peer.cur_addr.is_empty() {
        NetPath::Direct
    } else if peer.active && !peer.relay.is_empty() {
        NetPath::Relay(peer.relay.clone())
    } else {
        NetPath::Idle
    };
    Presence::Online { path }
}

/// `2026-10-08T14:02:11.123Z` -> `2026-10-08 14:02`.
fn short_time(raw: &str) -> String {
    match raw.get(..16) {
        Some(prefix) => prefix.replacen('T', " ", 1),
        None => raw.to_string(),
    }
}

/// Match a host by `tailscale_name`, else by canonical name, `ssh_target`,
/// or `display_target` against the peer's HostName or MagicDNS name. When several
/// peers match: without `tailscale_name` an online peer wins, with it a MagicDNS
/// match wins.
fn find_peer<'a>(config: &FleetConfig, name: &str, status: &'a Status) -> Option<&'a Peer> {
    let host = config.hosts.get(name)?;
    let candidates: Vec<String> = match &host.tailscale_name {
        Some(explicit) => vec![explicit.to_ascii_lowercase()],
        None => {
            let mut names = vec![
                name.to_ascii_lowercase(),
                host.ssh_target.to_ascii_lowercase(),
            ];
            if let Some(display) = &host.display_target {
                names.push(display.to_ascii_lowercase());
            }
            names
        }
    };
    let pinned = host.tailscale_name.is_some();
    status
        .peer
        .values()
        .filter_map(|peer| {
            let dns = peer.dns_name.trim_end_matches('.').to_ascii_lowercase();
            let dns_short = dns.split('.').next().unwrap_or_default().to_string();
            let host_name = peer.host_name.to_ascii_lowercase();
            let usable = |candidate: &&String| !candidate.is_empty();
            let dns_match = candidates
                .iter()
                .filter(usable)
                .any(|candidate| *candidate == dns || *candidate == dns_short);
            let host_match = candidates
                .iter()
                .filter(usable)
                .any(|candidate| *candidate == host_name);
            if !dns_match && !host_match {
                return None;
            }
            let key = if pinned {
                (dns_match, peer.online)
            } else {
                (peer.online, dns_match)
            };
            Some((key, peer))
        })
        .rev()
        .max_by_key(|(key, _)| *key)
        .map(|(_, peer)| peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config;

    const TOML: &str = r#"
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
ssh_target = "wb"
display_target = "workbox.tail0000.ts.net"
aliases = []
os = "linux"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = true

[hosts.relayed]
ssh_target = "relayed"
aliases = []
os = "linux"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = true

[hosts.sleepy]
ssh_target = "sleepy"
tailscale_name = "old-mini"
aliases = []
os = "darwin"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = false

[hosts.stranger]
ssh_target = "stranger"
aliases = []
os = "linux"
role = "compute"
user = "developer"
client_enrolled = true
gui = false
long_running_agents = false
"#;

    const STATUS: &str = r#"{
  "Self": {"HostName": "laptop", "DNSName": "laptop.tail0000.ts.net.", "Online": true},
  "Peer": {
    "nodekey:a": {"HostName": "WorkBox", "DNSName": "workbox.tail0000.ts.net.", "Online": true, "Active": true, "CurAddr": "192.0.2.4:41641", "Relay": "fra"},
    "nodekey:b": {"HostName": "relayed", "DNSName": "relayed.tail0000.ts.net.", "Online": true, "Active": true, "CurAddr": "", "Relay": "ams"},
    "nodekey:c": {"HostName": "old-mini", "DNSName": "old-mini.tail0000.ts.net.", "Online": false, "LastSeen": "2026-10-01T08:30:00Z"}
  }
}"#;

    #[test]
    fn maps_hosts_to_presence() {
        let config = parse_config(TOML).unwrap();
        let status = parse_status(STATUS).unwrap();
        let presence = presence_for(&config, Some(&status));
        assert_eq!(presence["laptop"], Presence::This);
        assert_eq!(
            presence["workbox"],
            Presence::Online {
                path: NetPath::Direct
            }
        );
        assert_eq!(
            presence["relayed"],
            Presence::Online {
                path: NetPath::Relay("ams".into())
            }
        );
        assert_eq!(
            presence["sleepy"],
            Presence::Offline {
                last_seen: Some("2026-10-01 08:30".into())
            }
        );
        assert_eq!(presence["stranger"], Presence::NotInTailnet);
        assert_eq!(presence["sleepy"].online(), Some(false));
    }

    #[test]
    fn missing_status_is_unknown_except_for_self() {
        let config = parse_config(TOML).unwrap();
        let presence = presence_for(&config, None);
        assert_eq!(presence["laptop"], Presence::This);
        assert_eq!(presence["workbox"], Presence::Unknown);
        assert_eq!(presence["workbox"].online(), None);
    }

    #[test]
    fn a_stopped_backend_is_not_evidence_about_peers() {
        let stopped = parse_status(r#"{"BackendState": "Stopped", "Peer": {}}"#).unwrap();
        assert!(!stopped.backend_running());
        assert!(parse_status(STATUS).unwrap().backend_running());
        assert!(parse_status(r#"{"BackendState": "Running"}"#)
            .unwrap()
            .backend_running());
    }

    #[test]
    fn tolerates_garbage() {
        assert!(parse_status("not json").is_none());
        assert!(parse_status("{}").unwrap().peer.is_empty());
    }

    const STALE: &str = r#""nodekey:a": {"HostName":"workbox","DNSName":"workbox.tail0000.ts.net.","Online":false,"LastSeen":"2026-10-01T08:30:00Z"}"#;
    const LIVE: &str = r#""nodekey:z": {"HostName":"workbox","DNSName":"workbox-1.tail0000.ts.net.","Online":true,"Active":true,"CurAddr":"192.0.2.9:41641"}"#;

    fn status_with(peers: &[&str]) -> Status {
        parse_status(&format!(r#"{{"Peer": {{{}}}}}"#, peers.join(","))).unwrap()
    }

    fn pinned_toml(name: &str) -> String {
        TOML.replacen(
            "[hosts.workbox]\n",
            &format!("[hosts.workbox]\ntailscale_name = \"{name}\"\n"),
            1,
        )
    }

    #[test]
    fn a_stale_offline_node_does_not_hide_the_live_one() {
        let config = parse_config(TOML).unwrap();
        let status = status_with(&[STALE, LIVE]);
        let presence = presence_for(&config, Some(&status));
        assert_eq!(
            presence["workbox"],
            Presence::Online {
                path: NetPath::Direct
            }
        );
    }

    #[test]
    fn an_explicit_tailscale_name_pins_the_dns_named_node() {
        let status = status_with(&[STALE, LIVE]);
        let live = parse_config(&pinned_toml("workbox-1")).unwrap();
        assert_eq!(
            presence_for(&live, Some(&status))["workbox"],
            Presence::Online {
                path: NetPath::Direct
            }
        );
        let stale = parse_config(&pinned_toml("workbox")).unwrap();
        assert!(matches!(
            presence_for(&stale, Some(&status))["workbox"],
            Presence::Offline { .. }
        ));
    }

    #[test]
    fn equal_matches_keep_node_key_order() {
        let config = parse_config(TOML).unwrap();
        let status = status_with(&[
            r#""nodekey:a": {"HostName":"workbox","DNSName":"one.tail0000.ts.net.","Online":true,"Active":true,"CurAddr":"","Relay":"fra"}"#,
            r#""nodekey:b": {"HostName":"workbox","DNSName":"two.tail0000.ts.net.","Online":true,"Active":true,"CurAddr":"","Relay":"ams"}"#,
        ]);
        let presence = presence_for(&config, Some(&status));
        assert_eq!(
            presence["workbox"],
            Presence::Online {
                path: NetPath::Relay("fra".into())
            }
        );
    }
}
