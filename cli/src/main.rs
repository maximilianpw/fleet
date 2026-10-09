use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use fleet::agents::{parse_hook_args, run_hook, AgentEnv, AgentError};
use fleet::config::{load_config, parse_port, validate_ssh_target};
use fleet::copy::{plan_copy_with, CopyOptions};
use fleet::forwards::{
    collect_forward_rows, ensure_ports_free, parse_forward_pids, render_forward_list,
    stop_forwards, stopped_message,
};
use fleet::moving::{run_move, MoveRequest};
use fleet::process::{exec_replace, PathKill, PathPsTable, ProcessEnv, ProcessTable};
use fleet::remote::Runner;
use fleet::select::HostFilter;
use fleet::ssh::{
    local_ports_of, parse_ssh_tail, plan_ad_hoc_forward, plan_run, plan_shell, plan_ssh, plan_t3,
};
use fleet::{
    render_list_json, run_doctor_command, run_local_agents, run_pick_command, run_ports_command,
    run_status_command, run_tunnel_command, write_json, write_stdout, ConfiguredInspector,
    FleetError, TunnelCommand, USAGE,
};

#[derive(Debug, Parser)]
#[command(
    name = "fleet",
    version,
    about = "SSH, tmux, and localhost forwards for a Fleet of machines",
    after_help = USAGE
)]
struct Cli {
    /// Read this TOML file instead of FLEET_CONFIG or the XDG/HOME default.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print the host table. This is also the default when no command is given.
    List {
        /// Print hosts as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show each host's Tailscale presence, tmux sessions, and agent states.
    ///
    /// Queries hosts concurrently over reused SSH connections. Hosts that
    /// Tailscale reports offline are not contacted.
    Status {
        /// Canonical names or aliases. Default: every host.
        hosts: Vec<String>,
        /// Filter hosts, e.g. `os=linux,long_running_agents,!local,online`.
        #[arg(long = "where", value_name = "EXPR")]
        filter: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List agents that reported through `fleet hook`, on every host.
    Agents {
        /// Canonical names or aliases. Default: every host.
        hosts: Vec<String>,
        #[arg(long = "where", value_name = "EXPR")]
        filter: Option<String>,
        /// Only this machine's records. Does not read Fleet configuration.
        #[arg(long, conflicts_with_all = ["hosts", "filter"])]
        local: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the first host matching --where, for use as `fleet run "$(fleet pick ...)" ...`.
    Pick {
        #[arg(long = "where", value_name = "EXPR")]
        filter: Option<String>,
        /// Print every matching host, one per line.
        #[arg(long)]
        all: bool,
    },
    /// Attach to a tmux session over SSH, or local tmux on the current host.
    Ssh {
        host: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Open a login shell. Trailing arguments after HOST are passed to ssh.
    Shell {
        host: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        ssh_args: Vec<String>,
    },
    /// Run a command. Local argv is preserved; remote argv is joined by OpenSSH.
    Run {
        host: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Copy a file, or a directory with -r, between this machine and a declared remote Fleet host.
    ///
    /// A bare destination host copies into the remote user's home directory.
    /// Use HOST:PATH to choose a remote path. Exactly one endpoint must be
    /// local; remote-to-remote copies are not supported.
    #[command(
        after_help = "Examples:\n  fleet copy report.md workbox\n  fleet copy report.md workbox:/tmp/report.md\n  fleet copy workbox:/tmp/report.md .\n  fleet copy -r workbox:/tmp/build ."
    )]
    Copy {
        /// Copy directories recursively (`scp -r`).
        #[arg(short = 'r', long)]
        recursive: bool,
        /// Local path, or HOST:PATH when pulling from a declared Fleet host.
        #[arg(allow_hyphen_values = true)]
        source: String,
        /// Local path, HOST:PATH, or a bare declared host for its home directory.
        #[arg(allow_hyphen_values = true)]
        destination: String,
    },
    /// Ad-hoc SSH local forwards, plus list/stop of observed forward processes.
    Forward {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Forward a host's declared T3 Code port to the local loopback.
    T3 {
        host: String,
        local_port: Option<String>,
    },
    /// Show, pause, or resume supervised localhost tunnels.
    #[command(subcommand_required = false)]
    Tunnel {
        #[command(subcommand)]
        command: Option<TunnelCli>,
    },
    /// List TCP ports listening on a host, ready for `fleet forward`.
    Ports {
        host: String,
        #[arg(long)]
        json: bool,
    },
    /// Check SSH reachability, tunnel health, and remote tmux/fleet.
    ///
    /// Without HOST, checks every host (or those matching --where) concurrently.
    Doctor {
        host: Option<String>,
        #[arg(long = "where", value_name = "EXPR", conflicts_with = "host")]
        filter: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Move an idle agent session to another host, resuming its conversation.
    ///
    /// The agent must have reported through `fleet hook`, be done or idle,
    /// and its checkout must be committed and pushed. TARGET gets the same
    /// home-relative checkout (cloned if missing, fast-forward only) and a
    /// detached tmux session running the agent's resume command. The source
    /// session ends only after the target session is running.
    Move {
        host: String,
        session: String,
        target: String,
        /// Leave the source session running.
        #[arg(long)]
        keep: bool,
        /// Move even if the agent is running or waiting for input.
        #[arg(long)]
        force: bool,
    },
    /// Report agent state from an agent hook. Exits 1 on error, never 2.
    #[command(disable_help_flag = true)]
    Hook {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Configuration helpers.
    #[command(subcommand)]
    Config(ConfigCli),
    /// Print shell completions to stdout. Does not read configuration.
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCli {
    /// Parse and validate configuration. Exit 0 if valid, 2 if invalid.
    Validate,
}

#[derive(Debug, Subcommand)]
enum TunnelCli {
    #[command(visible_alias = "ls")]
    Status {
        #[arg(long)]
        json: bool,
    },
    Pause {
        port: String,
    },
    Resume {
        port: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = error.to_string();
            if !message.is_empty() {
                eprintln!("{message}");
            }
            ExitCode::from(error.exit_code() as u8)
        }
    }
}

fn run(cli: Cli) -> Result<(), FleetError> {
    let env = ProcessEnv::from_os();
    match cli.command {
        Some(Commands::Completions { shell }) => {
            let mut script = Vec::new();
            generate(shell, &mut Cli::command(), "fleet", &mut script);
            write_stdout(script)
        }
        Some(Commands::Config(ConfigCli::Validate)) => {
            load_config(cli.config.as_deref(), &env)?;
            Ok(())
        }
        None => {
            let config = load_config(cli.config.as_deref(), &env)?;
            write_stdout(config.render_list())
        }
        Some(Commands::List { json }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            if json {
                write_json(&render_list_json(&config))
            } else {
                write_stdout(config.render_list())
            }
        }
        Some(Commands::Status {
            hosts,
            filter,
            json,
        }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let filter = filter.as_deref().map(HostFilter::parse).transpose()?;
            run_status_command(&config, &hosts, filter.as_ref(), &env, json, false)
        }
        Some(Commands::Agents {
            hosts,
            filter,
            local,
            json,
        }) => {
            if local {
                return run_local_agents(json);
            }
            let config = load_config(cli.config.as_deref(), &env)?;
            let filter = filter.as_deref().map(HostFilter::parse).transpose()?;
            run_status_command(&config, &hosts, filter.as_ref(), &env, json, true)
        }
        Some(Commands::Pick { filter, all }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let parsed = filter.as_deref().map(HostFilter::parse).transpose()?;
            run_pick_command(&config, parsed.as_ref(), filter.as_deref(), all)
        }
        Some(Commands::Ports { host, json }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            validate_ssh_target(&host)?;
            run_ports_command(&config, &host, &env, json)
        }
        Some(Commands::Move {
            host,
            session,
            target,
            keep,
            force,
        }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let request = MoveRequest {
                host,
                session,
                target,
                keep,
                force,
            };
            run_move(
                &config,
                &request,
                &Runner::new(&env),
                &AgentEnv::from_os(),
                &mut std::io::stdout(),
            )?;
            Ok(())
        }
        Some(Commands::Hook { args }) => {
            let result =
                parse_hook_args(&args).and_then(|parsed| run_hook(parsed, &AgentEnv::from_os()));
            match result {
                Err(AgentError::Usage) if args.iter().any(|a| a == "-h" || a == "--help") => {
                    write_stdout(format!("{}\n", fleet::agents::HOOK_USAGE))
                }
                other => Ok(other?),
            }
        }
        Some(Commands::Ssh { host, args }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let (session, forwards) = parse_ssh_tail(&args)?;
            let planned = plan_ssh(&config, &host, session.as_deref(), &forwards)?;
            if !forwards.is_empty() {
                let table = PathPsTable { env: &env };
                ensure_ports_free(&local_ports_of(&forwards), &table.list()?)?;
            }
            exec_replace(&planned)?;
            Ok(())
        }
        Some(Commands::Shell { host, ssh_args }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let planned = plan_shell(&config, &host, &ssh_args, &env)?;
            exec_replace(&planned)?;
            Ok(())
        }
        Some(Commands::Run { host, command }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let planned = plan_run(&config, &host, &command)?;
            exec_replace(&planned)?;
            Ok(())
        }
        Some(Commands::Copy {
            recursive,
            source,
            destination,
        }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let planned =
                plan_copy_with(&config, &source, &destination, CopyOptions { recursive })?;
            exec_replace(&planned)?;
            Ok(())
        }
        Some(Commands::Forward { args }) => dispatch_forward(&cli.config, &env, &args),
        Some(Commands::T3 { host, local_port }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let local_port = match local_port {
                Some(raw) => Some(parse_port(&raw)?),
                None => None,
            };
            let planned = plan_t3(&config, &host, local_port)?;
            let table = PathPsTable { env: &env };
            let port = local_port.unwrap_or_else(|| {
                config
                    .resolve(&host)
                    .and_then(|resolved| resolved.t3code_port)
                    .expect("plan_t3 already required a T3 port")
            });
            ensure_ports_free(&[port], &table.list()?)?;
            exec_replace(&planned)?;
            Ok(())
        }
        Some(Commands::Tunnel { command }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let command = match command {
                None => TunnelCommand::Status { json: false },
                Some(TunnelCli::Status { json }) => TunnelCommand::Status { json },
                Some(TunnelCli::Pause { port }) => TunnelCommand::Pause {
                    port: parse_port(&port)?,
                },
                Some(TunnelCli::Resume { port }) => TunnelCommand::Resume {
                    port: parse_port(&port)?,
                },
            };
            run_tunnel_command(&config, command, &env)
        }
        Some(Commands::Doctor { host, filter, json }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            let explicit_host = host.is_some();
            let hosts = match host {
                Some(host) => {
                    validate_ssh_target(&host)?;
                    if config.resolve(&host).is_none() {
                        return Err(fleet::SshError::UnknownHost(host).into());
                    }
                    vec![host]
                }
                None => {
                    let parsed = filter.as_deref().map(HostFilter::parse).transpose()?;
                    let presence = fleet::presence_for_filter(&config, parsed.as_ref());
                    let hosts = fleet::select_hosts(&config, &[], parsed.as_ref(), &presence)?;
                    if hosts.is_empty() {
                        return Err(FleetError::NoMatch(filter.unwrap_or_default()));
                    }
                    hosts
                }
            };
            run_doctor_command(&config, &hosts, &env, json, explicit_host)
        }
    }
}

fn dispatch_forward(
    cli_config: &Option<PathBuf>,
    env: &ProcessEnv,
    args: &[String],
) -> Result<(), FleetError> {
    match args {
        [] => Err(FleetError::usage()),
        [cmd, rest @ ..] if matches!(cmd.as_str(), "list" | "ls") => {
            let _config = load_config(cli_config.as_deref(), env)?;
            let json = rest.iter().any(|arg| arg == "--json");
            let rest: Vec<&String> = rest.iter().filter(|arg| *arg != "--json").collect();
            if rest.len() > 1 {
                return Err(FleetError::usage());
            }
            let port = match rest.first() {
                Some(raw) => Some(parse_port(raw)?),
                None => None,
            };
            let table = PathPsTable { env };
            let rows = collect_forward_rows(&table.list()?);
            if json {
                let rows: Vec<_> = rows
                    .iter()
                    .filter(|row| port.is_none_or(|port| row.local_port == port))
                    .collect();
                write_json(&rows)
            } else {
                write_stdout(render_forward_list(&rows, port))
            }
        }
        [cmd, rest @ ..] if matches!(cmd.as_str(), "stop" | "delete" | "rm") => {
            let config = load_config(cli_config.as_deref(), env)?;
            let pids = parse_forward_pids(rest)?;
            let table = PathPsTable { env };
            let inspector = ConfiguredInspector {
                config: &config,
                env,
            };
            let signals = PathKill { env };
            let stopped = stop_forwards(&pids, &table, &inspector, &signals)?;
            let report: String = stopped
                .into_iter()
                .map(|pid| stopped_message(pid) + "\n")
                .collect();
            write_stdout(report)
        }
        _ => {
            if args.len() < 3 || args.len() > 4 {
                return Err(FleetError::usage());
            }
            let host = &args[0];
            let local_port = parse_port(&args[1])?;
            let remote_port = parse_port(&args[2])?;
            let remote_host = args.get(3).map(String::as_str).unwrap_or("localhost");
            let config = load_config(cli_config.as_deref(), env)?;
            let planned = plan_ad_hoc_forward(&config, host, local_port, remote_port, remote_host)?;
            let table = PathPsTable { env };
            ensure_ports_free(&[local_port], &table.list()?)?;
            exec_replace(&planned)?;
            Ok(())
        }
    }
}
