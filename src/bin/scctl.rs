use clap::{Parser, Subcommand};
use scd::{Client, Config};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Control the Steam Controller daemon")]
struct Args {
    #[arg(long, default_value = "/run/scd/control.sock")]
    socket: PathBuf,
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
        #[arg(default_value = "/etc/scd/config.toml")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum ModeAction {
    Set { name: String },
    Next,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if let Command::Validate { path } = args.command {
        Config::load(path)?;
        println!("configuration is valid");
        return Ok(());
    }

    let client = Client::new(args.socket);
    match args.command {
        Command::Status { json: true } => println!("{}", serde_json::to_string(&client.status()?)?),
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
                println!("{}", serde_json::to_string(&event?)?);
            }
        }
        Command::Validate { .. } => unreachable!(),
    }
    Ok(())
}
