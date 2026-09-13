//! Ad-hoc SSH local-forward discovery, listing, and deletion.
//!
//! Discovery parses observed `ps` argv, not reconstructed shell quoting. Both
//! `-L spec` and `-Lspec` forms are supported. A stored PID is never enough to
//! delete: ownership is re-observed immediately before signaling. Managed
//! protection uses [`ManagedForwardInspector`]; supervisor `none` never
//! fabricates ownership. With `launchd` and no job snapshot, a mapped port is
//! treated as ambiguous rather than guessed.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::config::{FleetConfig, PortError, Supervisor};
use crate::process::{ObservedProcess, ProcessError, ProcessTable, SignalSender};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRow {
    pub pid: u32,
    pub local_port: u16,
    pub spec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedClass {
    Unmanaged,
    Managed { port: u16, label: String },
    Ambiguous,
}

pub trait ManagedForwardInspector {
    fn classify(&self, pid: u32, local_port: u16) -> ManagedClass;
}

/// Production inspector until launchd job/child identity lands in Stage C.
pub struct ConservativeManagedInspector {
    supervisor: Supervisor,
    mapped_ports: Vec<u16>,
}

impl ConservativeManagedInspector {
    pub fn from_config(config: &FleetConfig) -> Self {
        Self {
            supervisor: config.tunnels.supervisor,
            mapped_ports: config
                .tunnels
                .mappings
                .iter()
                .map(|mapping| mapping.local_port.get())
                .collect(),
        }
    }
}

impl ManagedForwardInspector for ConservativeManagedInspector {
    fn classify(&self, _pid: u32, local_port: u16) -> ManagedClass {
        match self.supervisor {
            Supervisor::None => ManagedClass::Unmanaged,
            Supervisor::Launchd if self.mapped_ports.contains(&local_port) => {
                ManagedClass::Ambiguous
            }
            Supervisor::Launchd => ManagedClass::Unmanaged,
        }
    }
}

/// Test inspector that returns a class by PID, else `default`.
#[derive(Debug, Clone)]
pub struct SnapshotInspector {
    pub class_by_pid: BTreeMap<u32, ManagedClass>,
    pub default: ManagedClass,
}

impl ManagedForwardInspector for SnapshotInspector {
    fn classify(&self, pid: u32, _local_port: u16) -> ManagedClass {
        self.class_by_pid
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }
}

#[derive(Debug, Error)]
pub enum ForwardError {
    #[error("fleet: expected one or more forward PIDs")]
    MissingPids,
    #[error("fleet: forward PID must be numeric: {0}")]
    PidNotNumeric(String),
    #[error("fleet: PID {0} is not an active SSH local forward")]
    PidNotForward(u32),
    #[error("{0}")]
    PortBusy(String),
    #[error(
        "fleet: PID {pid} is a managed tunnel; use fleet tunnel pause {port} instead of deleting it"
    )]
    ManagedPid { pid: u32, port: u16 },
    #[error(
        "fleet: refuse to delete PID {pid}: managed tunnel ownership could not be established"
    )]
    AmbiguousManaged { pid: u32 },
    #[error("fleet: failed to stop SSH forward process {pid}")]
    StopFailed { pid: u32 },
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Process(#[from] ProcessError),
}

impl ForwardError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::MissingPids | Self::PidNotNumeric(_) | Self::Port(_) => 2,
            _ => 1,
        }
    }
}

/// Extract the local port from an OpenSSH `-L` spec the way the legacy script did.
pub fn forward_local_port(spec: &str) -> Option<u16> {
    let (first, rest) = spec.split_once(':')?;
    let port_token = if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        rest.split(':').next().unwrap_or("")
    } else {
        first
    };
    if port_token.is_empty() || !port_token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    port_token.parse().ok()
}

pub fn collect_forward_rows(processes: &[ObservedProcess]) -> Vec<ForwardRow> {
    let mut rows = Vec::new();
    for process in processes {
        if !is_ssh(&process.argv) {
            continue;
        }
        let mut index = 1;
        while index < process.argv.len() {
            let arg = &process.argv[index];
            if arg == "-L" {
                if let Some(spec) = process.argv.get(index + 1) {
                    if let Some(local_port) = forward_local_port(spec) {
                        rows.push(ForwardRow {
                            pid: process.pid,
                            local_port,
                            spec: spec.clone(),
                        });
                    }
                    index += 2;
                    continue;
                }
                break;
            }
            if let Some(spec) = arg.strip_prefix("-L") {
                if !spec.is_empty() {
                    if let Some(local_port) = forward_local_port(spec) {
                        rows.push(ForwardRow {
                            pid: process.pid,
                            local_port,
                            spec: spec.to_string(),
                        });
                    }
                }
            }
            index += 1;
        }
    }
    rows
}

fn is_ssh(argv: &[String]) -> bool {
    argv.first()
        .is_some_and(|program| program == "ssh" || program.ends_with("/ssh"))
}

pub fn rows_for_port(rows: &[ForwardRow], port: u16) -> Vec<&ForwardRow> {
    rows.iter().filter(|row| row.local_port == port).collect()
}

pub fn port_has_forward(rows: &[ForwardRow], port: u16) -> bool {
    rows.iter().any(|row| row.local_port == port)
}

pub fn render_forward_list(rows: &[ForwardRow], filter_port: Option<u16>) -> String {
    let mut out = String::new();
    if let Some(port) = filter_port {
        out.push_str(&format!("Active SSH local forwards for port {port}:\n"));
    } else {
        out.push_str("Active SSH local forwards:\n");
    }
    out.push_str(&format!(
        "{:<8} {:<10} {:<36} DELETE_COMMAND\n",
        "PID", "LOCAL_PORT", "FORWARD"
    ));
    let mut found = false;
    for row in rows {
        if let Some(port) = filter_port {
            if row.local_port != port {
                continue;
            }
        }
        found = true;
        out.push_str(&format!(
            "{:<8} {:<10} {:<36} fleet forward delete {}\n",
            row.pid, row.local_port, row.spec, row.pid
        ));
    }
    if !found {
        if let Some(port) = filter_port {
            out.push_str(&format!(
                "No active SSH local forwards found for local port {port}.\n"
            ));
        } else {
            out.push_str("No active SSH local forwards found.\n");
        }
    }
    out
}

pub fn port_busy_error(port: u16, rows: &[ForwardRow]) -> ForwardError {
    let mut message = format!("fleet: local port {port} already has an active SSH forward.\n");
    message.push_str("fleet: stop the existing forward before opening another one:\n");
    message.push_str(&render_forward_list(rows, Some(port)));
    // render_forward_list already ends with a newline; trim the extra blank from Display users.
    ForwardError::PortBusy(message.trim_end().to_string())
}

pub fn ensure_ports_free(ports: &[u16], processes: &[ObservedProcess]) -> Result<(), ForwardError> {
    let rows = collect_forward_rows(processes);
    for port in ports {
        if port_has_forward(&rows, *port) {
            return Err(port_busy_error(*port, &rows));
        }
    }
    Ok(())
}

pub fn parse_forward_pids(raw: &[String]) -> Result<Vec<u32>, ForwardError> {
    if raw.is_empty() {
        return Err(ForwardError::MissingPids);
    }
    let mut pids = Vec::new();
    for token in raw {
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ForwardError::PidNotNumeric(token.clone()));
        }
        let pid: u32 = token
            .parse()
            .map_err(|_| ForwardError::PidNotNumeric(token.clone()))?;
        pids.push(pid);
    }
    Ok(pids)
}

pub fn stop_forwards<T: ProcessTable, S: SignalSender>(
    pids: &[u32],
    table: &T,
    inspector: &dyn ManagedForwardInspector,
    signals: &S,
) -> Result<Vec<u32>, ForwardError> {
    if pids.is_empty() {
        return Err(ForwardError::MissingPids);
    }
    let initial = collect_forward_rows(&table.list()?);
    for pid in pids {
        let rows: Vec<&ForwardRow> = initial.iter().filter(|row| row.pid == *pid).collect();
        if rows.is_empty() {
            return Err(ForwardError::PidNotForward(*pid));
        }
        refuse_managed(*pid, &rows, inspector)?;
    }

    let mut stopped = Vec::new();
    for pid in pids {
        let live = collect_forward_rows(&table.list()?);
        let rows: Vec<&ForwardRow> = live.iter().filter(|row| row.pid == *pid).collect();
        if rows.is_empty() {
            return Err(ForwardError::PidNotForward(*pid));
        }
        refuse_managed(*pid, &rows, inspector)?;
        signals
            .signal(*pid)
            .map_err(|_| ForwardError::StopFailed { pid: *pid })?;
        stopped.push(*pid);
    }
    Ok(stopped)
}

fn refuse_managed(
    pid: u32,
    rows: &[&ForwardRow],
    inspector: &dyn ManagedForwardInspector,
) -> Result<(), ForwardError> {
    for row in rows {
        match inspector.classify(pid, row.local_port) {
            ManagedClass::Unmanaged => {}
            ManagedClass::Managed { port, .. } => {
                return Err(ForwardError::ManagedPid { pid, port });
            }
            ManagedClass::Ambiguous => {
                return Err(ForwardError::AmbiguousManaged { pid });
            }
        }
    }
    Ok(())
}
