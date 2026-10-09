//! `fleet ports HOST`: TCP ports listening on a host, ready to forward.
//!
//! The probe prefers `ss` and falls back to `lsof`, so it works on Linux and
//! macOS without trusting the free-form `os` field.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Serialize;

use crate::remote::REMOTE_PATH_SETUP;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ListeningPort {
    pub port: u16,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

pub fn ports_script() -> String {
    format!(
        ": fleet-ports; {REMOTE_PATH_SETUP}; \
if command -v ss >/dev/null 2>&1; then echo @ss; ss -ltnpH 2>/dev/null; \
elif command -v lsof >/dev/null 2>&1; then echo @lsof; lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null; \
else echo @none; fi"
    )
}

/// Parse the probe output. `None` when the host had neither `ss` nor `lsof`.
pub fn parse_ports(stdout: &str) -> Option<Vec<ListeningPort>> {
    let mut lines = stdout.lines();
    let parser: fn(&str) -> Option<ListeningPort> = loop {
        match lines.next()?.trim() {
            "@ss" => break parse_ss_line,
            "@lsof" => break parse_lsof_line,
            "@none" => return None,
            _ => continue,
        }
    };
    let unique: BTreeSet<ListeningPort> = lines.filter_map(parser).collect();
    Some(unique.into_iter().collect())
}

/// `LISTEN 0 4096 127.0.0.1:5173 0.0.0.0:* users:(("node",pid=812,fd=21))`
fn parse_ss_line(line: &str) -> Option<ListeningPort> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let local = fields.get(3)?;
    let (address, port) = split_address(local)?;
    let users = fields.iter().find(|field| field.starts_with("users:"));
    let process = users.and_then(|users| {
        let start = users.find("((\"")? + 3;
        let end = users[start..].find('"')? + start;
        Some(users[start..end].to_string())
    });
    let pid = users.and_then(|users| {
        let start = users.find("pid=")? + 4;
        let digits: String = users[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    });
    Some(ListeningPort {
        port,
        address,
        process,
        pid,
    })
}

/// `node 812 dev 21u IPv4 0xabc 0t0 TCP 127.0.0.1:5173 (LISTEN)`
fn parse_lsof_line(line: &str) -> Option<ListeningPort> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.first() == Some(&"COMMAND") {
        return None;
    }
    let name_index = fields.iter().position(|field| *field == "TCP")? + 1;
    let (address, port) = split_address(fields.get(name_index)?)?;
    Some(ListeningPort {
        port,
        address,
        process: fields.first().map(|name| name.to_string()),
        pid: fields.get(1).and_then(|pid| pid.parse().ok()),
    })
}

/// `127.0.0.1:5173`, `*:22`, `[::1]:3000`, `[::]:22` -> (address, port).
fn split_address(raw: &str) -> Option<(String, u16)> {
    let (address, port) = raw.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let address = address.trim_start_matches('[').trim_end_matches(']');
    let address = if address.is_empty() { "*" } else { address };
    Some((address.to_string(), port))
}

pub fn render_ports(host: &str, ports: &[ListeningPort]) -> String {
    let mut out = String::new();
    if ports.is_empty() {
        let _ = writeln!(out, "No listening TCP ports reported on {host}.");
        return out;
    }
    let _ = writeln!(out, "{:<7} {:<24} PROCESS", "PORT", "ADDRESS");
    let mut seen = BTreeSet::new();
    for port in ports {
        let process = match (&port.process, port.pid) {
            (Some(name), Some(pid)) => format!("{name} ({pid})"),
            (Some(name), None) => name.clone(),
            (None, Some(pid)) => format!("pid {pid}"),
            (None, None) => "-".into(),
        };
        let _ = writeln!(out, "{:<7} {:<24} {}", port.port, port.address, process);
        seen.insert(port.port);
    }
    if let Some(first) = seen.iter().next() {
        let _ = writeln!(
            out,
            "\nforward one with: fleet forward {host} {first} {first}"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ss_output() {
        let stdout = "@ss\n\
LISTEN 0 4096 127.0.0.1:5173 0.0.0.0:* users:((\"node\",pid=812,fd=21))\n\
LISTEN 0 128 [::]:22 [::]:*\n\
LISTEN 0 4096 [::1]:5173 [::]:* users:((\"node\",pid=812,fd=22))\n";
        let ports = parse_ports(stdout).unwrap();
        assert_eq!(ports.len(), 3);
        assert_eq!(ports[0].port, 22);
        assert_eq!(ports[0].address, "::");
        assert_eq!(ports[0].process, None);
        let node = ports.iter().find(|p| p.address == "127.0.0.1").unwrap();
        assert_eq!(node.process.as_deref(), Some("node"));
        assert_eq!(node.pid, Some(812));
    }

    #[test]
    fn parses_lsof_output() {
        let stdout = "noise from a login shell\n@lsof\n\
COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
node 812 dev 21u IPv4 0xabc 0t0 TCP 127.0.0.1:5173 (LISTEN)\n\
node 812 dev 22u IPv6 0xdef 0t0 TCP *:5173 (LISTEN)\n";
        let ports = parse_ports(stdout).unwrap();
        assert_eq!(ports.len(), 2);
        assert!(ports.iter().all(|p| p.pid == Some(812)));
        assert!(ports.iter().any(|p| p.address == "*"));
    }

    #[test]
    fn no_tools_is_none() {
        assert_eq!(parse_ports("@none\n"), None);
        assert_eq!(parse_ports(""), None);
    }

    #[test]
    fn script_has_no_backslashes() {
        assert!(!ports_script().contains('\\'));
    }
}
