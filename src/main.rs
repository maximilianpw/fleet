use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use fleet::config::{load_config, parse_port, validate_ssh_target};
use fleet::forwards::{
    collect_forward_rows, ensure_ports_free, parse_forward_pids, render_forward_list, stop_forwards,
};
use fleet::process::{exec_replace, PathKill, PathPsTable, ProcessEnv, ProcessTable};
use fleet::ssh::{
    local_ports_of, parse_ssh_tail, plan_ad_hoc_forward, plan_run, plan_shell, plan_ssh, plan_t3,
};
use fleet::{
    run_doctor_command, run_tunnel_command, ConfiguredInspector, FleetError, TunnelCommand, USAGE,
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
    List,
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
    /// Supervised localhost tunnels. Stage C implements pause/resume/status.
    #[command(subcommand_required = false)]
    Tunnel {
        #[command(subcommand)]
        command: Option<TunnelCli>,
    },
    /// Check SSH reachability and tunnel health. Stage C implements probes.
    Doctor { host: String },
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
    Status,
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
            generate(shell, &mut Cli::command(), "fleet", &mut io::stdout());
            Ok(())
        }
        Some(Commands::Config(ConfigCli::Validate)) => {
            load_config(cli.config.as_deref(), &env)?;
            Ok(())
        }
        None | Some(Commands::List) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            print!("{}", config.render_list());
            Ok(())
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
                None | Some(TunnelCli::Status) => TunnelCommand::Status,
                Some(TunnelCli::Pause { port }) => TunnelCommand::Pause {
                    port: parse_port(&port)?,
                },
                Some(TunnelCli::Resume { port }) => TunnelCommand::Resume {
                    port: parse_port(&port)?,
                },
            };
            run_tunnel_command(&config, command, &env)
        }
        Some(Commands::Doctor { host }) => {
            let config = load_config(cli.config.as_deref(), &env)?;
            validate_ssh_target(&host)?;
            if config.resolve(&host).is_none() {
                return Err(fleet::SshError::UnknownHost(host).into());
            }
            run_doctor_command(&config, &host, &env)
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
            if rest.len() > 1 {
                return Err(FleetError::usage());
            }
            let port = match rest.first() {
                Some(raw) => Some(parse_port(raw)?),
                None => None,
            };
            let table = PathPsTable { env };
            let rows = collect_forward_rows(&table.list()?);
            print!("{}", render_forward_list(&rows, port));
            Ok(())
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
            for pid in stopped {
                println!("fleet: stopped SSH forward process {pid}");
            }
            Ok(())
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
