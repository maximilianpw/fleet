//! Ad-hoc SSH local-forward discovery, listing, and deletion.
//!
//! Discovery parses observed `ps` argv, not reconstructed shell quoting. Both
//! `-L spec` and `-Lspec` forms are supported. A stored PID is never enough to
//! delete: ownership is re-observed immediately before signaling. Managed
//! protection uses [`ManagedForwardInspector`]; supervisor `none` never
//! fabricates ownership. With `launchd` and no job snapshot, a mapped port is
//! treated as ambiguous rather than guessed.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::config::{FleetConfig, PortError, Supervisor};
use crate::process::{ObservedProcess, ProcessError, ProcessTable, SignalSender};
use crate::{is_decimal, parse_decimal};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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

/// Mapped-port policy used when launchd identity cannot be read: under
/// `launchd`, any mapped port is ambiguous; under `none`, nothing is managed.
pub struct ConservativeManagedInspector {
    supervisor: Supervisor,
    mapped_ports: BTreeSet<u16>,
}

impl ConservativeManagedInspector {
    pub fn from_config(config: &FleetConfig) -> Self {
        Self {
            supervisor: config.tunnels.supervisor,
            mapped_ports: config.mapped_local_ports(),
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
    /// Some PIDs were signaled before `source` stopped the batch.
    #[error("{}", partial_stop_message(.stopped, .source))]
    PartiallyStopped {
        stopped: Vec<u32>,
        source: Box<ForwardError>,
    },
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Process(#[from] ProcessError),
}

impl ForwardError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::MissingPids | Self::PidNotNumeric(_) | Self::Port(_) => 2,
            Self::PartiallyStopped { source, .. } => source.exit_code(),
            _ => 1,
        }
    }
}

/// `fleet: stopped SSH forward process PID` line, shared by success output
/// and partial-failure reports.
pub fn stopped_message(pid: u32) -> String {
    format!("fleet: stopped SSH forward process {pid}")
}

fn partial_stop_message(stopped: &[u32], source: &ForwardError) -> String {
    let mut message = String::new();
    for pid in stopped {
        message.push_str(&stopped_message(*pid));
        message.push('\n');
    }
    message.push_str(&source.to_string());
    message
}

/// Extract the local port from an OpenSSH `-L` spec the way the legacy script did.
pub fn forward_local_port(spec: &str) -> Option<u16> {
    let (first, rest) = spec.split_once(':')?;
    let port_token = if is_decimal(first) {
        first
    } else {
        rest.split(':').next().unwrap_or("")
    };
    parse_decimal(port_token)
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

fn port_busy_error(port: u16, rows: &[ForwardRow]) -> ForwardError {
    let mut message = format!("fleet: local port {port} already has an active SSH forward.\n");
    message.push_str("fleet: stop the existing forward before opening another one:\n");
    message.push_str(&render_forward_list(rows, Some(port)));
    // render_forward_list already ends with a newline; trim the extra blank from Display users.
    ForwardError::PortBusy(message.trim_end().to_string())
}

pub fn ensure_ports_free(ports: &[u16], processes: &[ObservedProcess]) -> Result<(), ForwardError> {
    let rows = collect_forward_rows(processes);
    match ports
        .iter()
        .find(|port| rows.iter().any(|row| row.local_port == **port))
    {
        Some(port) => Err(port_busy_error(*port, &rows)),
        None => Ok(()),
    }
}

pub fn parse_forward_pids(raw: &[String]) -> Result<Vec<u32>, ForwardError> {
    if raw.is_empty() {
        return Err(ForwardError::MissingPids);
    }
    raw.iter()
        .map(|token| parse_decimal(token).ok_or_else(|| ForwardError::PidNotNumeric(token.clone())))
        .collect()
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
        if let Err(error) = reobserve_and_stop(*pid, table, inspector, signals) {
            return Err(if stopped.is_empty() {
                error
            } else {
                ForwardError::PartiallyStopped {
                    stopped,
                    source: Box::new(error),
                }
            });
        }
        stopped.push(*pid);
    }
    Ok(stopped)
}

/// Ownership is re-observed immediately before each signal; a PID checked
/// earlier in the batch may have exited or been replaced since.
fn reobserve_and_stop<T: ProcessTable, S: SignalSender>(
    pid: u32,
    table: &T,
    inspector: &dyn ManagedForwardInspector,
    signals: &S,
) -> Result<(), ForwardError> {
    let live = collect_forward_rows(&table.list()?);
    let rows: Vec<&ForwardRow> = live.iter().filter(|row| row.pid == pid).collect();
    if rows.is_empty() {
        return Err(ForwardError::PidNotForward(pid));
    }
    refuse_managed(pid, &rows, inspector)?;
    signals.signal(pid)?;
    Ok(())
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
