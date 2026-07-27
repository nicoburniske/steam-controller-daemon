use clap::{Parser, Subcommand, ValueEnum};
use scd::{Client, Config, HapticSound, ResultExt};
use std::{path::PathBuf, thread, time::Duration};

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
    Sound {
        #[arg(value_enum)]
        sound: Option<HapticSound>,
    },
    Reload,
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

fn main() -> scd::Result<()> {
    let args = Args::parse();
    let client = Client::new(args.socket);
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
        Command::Sound { sound: Some(sound) } => client.play_sound(sound)?,
        Command::Sound { sound: None } => {
            for &sound in HapticSound::value_variants() {
                println!("{}", sound.to_possible_value().unwrap().get_name());
                client.play_sound(sound)?;
                thread::sleep(Duration::from_millis(700));
            }
        }
        Command::Reload => client.reload()?,
        Command::Validate { path } => {
            Config::load(path)?;
            println!("configuration is valid");
        }
    }
    Ok(())
}
