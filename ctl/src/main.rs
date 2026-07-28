use clap::{Parser, Subcommand};
use scd::protocol::Haptic;
use scd::{Client, Config};
use std::fs;
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
    Haptic {
        #[command(subcommand)]
        haptic: HapticCommand,
    },
    Steam {
        #[command(subcommand)]
        action: SteamAction,
    },
    Reload,
    Validate {
        #[arg(default_value = "/etc/scd/config.toml")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum HapticCommand {
    /// play a fixed-frequency tone
    Tone {
        /// frequency in hertz
        frequency: u16,
        /// duration in milliseconds
        #[arg(long, default_value_t = 100)]
        duration_ms: u16,
        /// strength, where 0 is strongest and negative values are quieter
        #[arg(long, default_value_t = -10, allow_hyphen_values = true)]
        gain: i8,
    },
    /// sweep logarithmically between two frequencies
    Sweep {
        /// starting frequency in hertz
        start_frequency: u16,
        /// ending frequency in hertz
        end_frequency: u16,
        /// duration in milliseconds
        #[arg(long, default_value_t = 120)]
        duration_ms: u16,
        /// strength, where 0 is strongest and negative values are quieter
        #[arg(long, default_value_t = -10, allow_hyphen_values = true)]
        gain: i8,
    },
    /// alternate the haptics on and off
    Pulse {
        /// active time in microseconds
        #[arg(long, default_value_t = 625)]
        on_us: u16,
        /// inactive time in microseconds
        #[arg(long, default_value_t = 625)]
        off_us: u16,
        /// number of pulses
        #[arg(long, default_value_t = 48)]
        repeat: u16,
    },
    /// play a built-in controller script
    Script {
        /// firmware script number
        script: u8,
        /// strength, where 0 is strongest and negative values are quieter
        #[arg(long, default_value_t = -10, allow_hyphen_values = true)]
        gain: i8,
    },
}

#[derive(Subcommand)]
enum ModeAction {
    Set { name: String },
    Next,
}

#[derive(Subcommand)]
enum SteamAction {
    Enable,
    Disable,
}

fn main() -> scd::Result<()> {
    let args = Args::parse();
    let client = Client::new(args.socket);
    match args.command {
        Command::Status { json: true } => {
            println!("{}", serde_json::to_string(&client.status()?)?)
        }
        Command::Status { json: false } => {
            let status = client.status()?;
            println!("connected: {}", status.connected);
            println!("steam: {}", status.steam);
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
        Command::Mode { action: None } => println!("{}", client.status()?.mode),
        Command::Mode {
            action: Some(ModeAction::Set { name }),
        } => client.set_mode(name)?,
        Command::Mode {
            action: Some(ModeAction::Next),
        } => client.next_mode()?,
        Command::Haptic { haptic } => match haptic {
            HapticCommand::Tone {
                frequency,
                duration_ms,
                gain,
            } => client.play_haptic(Haptic::Tone {
                gain,
                frequency,
                duration_ms,
            })?,
            HapticCommand::Sweep {
                start_frequency,
                end_frequency,
                duration_ms,
                gain,
            } => client.play_haptic(Haptic::LogSweep {
                gain,
                duration_ms,
                start_frequency,
                end_frequency,
            })?,
            HapticCommand::Pulse {
                on_us,
                off_us,
                repeat,
            } => client.play_haptic(Haptic::Pulse {
                on_us,
                off_us,
                repeat,
            })?,
            HapticCommand::Script { script, gain } => {
                client.play_haptic(Haptic::Script { script, gain })?
            }
        },
        Command::Steam {
            action: SteamAction::Enable,
        } => client.set_steam(true)?,
        Command::Steam {
            action: SteamAction::Disable,
        } => client.set_steam(false)?,
        Command::Reload => client.reload()?,
        Command::Validate { path } => {
            Config::parse(&fs::read_to_string(path)?)?;
            println!("configuration is valid");
        }
    }
    Ok(())
}
