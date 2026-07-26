use clap::Parser;
use scd::{Daemon, paths};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Steam Controller userspace daemon")]
struct Args {
    #[arg(
        long,
        help = "Configuration path (default: $XDG_CONFIG_HOME/scd/config.toml or $HOME/.config/scd/config.toml)"
    )]
    config: Option<PathBuf>,
    #[arg(
        long,
        help = "Control socket path (default: $XDG_RUNTIME_DIR/scd/control.sock or /run/scd/control.sock)"
    )]
    socket: Option<PathBuf>,
}

fn main() -> scd::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    Daemon::new(paths::config(args.config)?, paths::socket(args.socket)).run()
}
