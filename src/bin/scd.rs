use clap::Parser;
use scd::Daemon;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Steam Controller userspace daemon")]
struct Args {
    #[arg(long, default_value = "/etc/scd/config.toml")]
    config: PathBuf,
    #[arg(long, default_value = "/run/scd/control.sock")]
    socket: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    Daemon::new(args.config, args.socket).run()?;
    Ok(())
}
