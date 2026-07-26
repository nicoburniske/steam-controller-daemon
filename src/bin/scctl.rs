use clap::{Parser, Subcommand};
use scd::{Client, Config, ResultExt, paths};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Control the Steam Controller daemon")]
struct Args {
    #[arg(
        long,
        help = "Control socket path (default: $XDG_RUNTIME_DIR/scd/control.sock or /run/scd/control.sock)"
    )]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Status {
        #[arg(long)]
        json: bool,
    },
    Mode {
        #[command(subcommand)]
        action: Option<ModeAction>,
    },
    Reload,
    Events,
    Validate {
        #[arg(
            help = "Configuration path (default: $XDG_CONFIG_HOME/scd/config.toml or $HOME/.config/scd/config.toml)"
        )]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ModeAction {
    Set { name: String },
    Next,
}

fn main() -> scd::Result<()> {
    let args = Args::parse();
    let client = Client::new(paths::socket(args.socket));
    match args.command {
        Command::Status { json: true } => {
            println!("{}", serde_json::to_string(&client.status()?).whence()?)
        }
        Command::Status { json: false } => {
            let status = client.status()?;
            println!("connected: {}", status.connected);
            println!("mode: {}", status.mode);
            if let Some(device) = status.device {
                println!("device: {device}");
            }
            if let Some(battery) = status.battery_percent {
                println!(
                    "battery: {battery}%{}",
                    if status.charging == Some(true) {
                        " (charging)"
                    } else {
                        ""
                    }
                );
            }
        }
        Command::Mode { action: None } => println!("{}", client.mode()?),
        Command::Mode {
            action: Some(ModeAction::Set { name }),
        } => client.set_mode(name)?,
        Command::Mode {
            action: Some(ModeAction::Next),
        } => client.next_mode()?,
        Command::Reload => client.reload()?,
        Command::Events => {
            for event in client.events()? {
                println!("{}", serde_json::to_string(&event?).whence()?);
            }
        }
        Command::Validate { path } => {
            Config::load(paths::config(path)?)?;
            println!("configuration is valid");
        }
    }
    Ok(())
}
