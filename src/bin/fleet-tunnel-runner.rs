//! Supervisor entry point. Argument contract: local port, then SSH argv.
//!
//! Nix LaunchAgents call this as:
//! `fleet-tunnel-runner PORT -o BatchMode=yes ... -N -L 127.0.0.1:PORT:HOST:REMOTE fleet-forward-HOST`

fn main() {
    fleet::runner::install_stop_handlers();
    let cfg = match fleet::runner::parse_args(std::env::args_os()) {
        Ok(cfg) => cfg,
        Err(code) => std::process::exit(code),
    };
    std::process::exit(fleet::runner::run(&cfg));
}
