mod devices;
mod display;
mod doctor;
mod exit;
mod info;
mod legacy;
mod media;
mod output;

use clap::{Parser, Subcommand};
use exit::{CommandResult, Failure};
use std::process::ExitCode;

/// Control TRYX cooler displays from the terminal.
#[derive(Parser)]
#[command(name = "tryx", version, about)]
struct Cli {
    /// Emit machine-readable JSON instead of human output.
    #[arg(long, global = true)]
    json: bool,

    /// Serial port of a legacy-firmware display, bypassing discovery.
    #[arg(long, global = true, value_name = "PATH")]
    tty: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check ffmpeg, USB permissions, and connected displays.
    Doctor,
    /// List connected TRYX displays.
    Devices,
    /// Identify the connected display.
    Info,
    /// Read or change display settings.
    Display {
        #[command(subcommand)]
        action: DisplayAction,
    },
    /// Manage media stored on the display.
    Media {
        #[command(subcommand)]
        action: MediaAction,
    },
}

#[derive(Subcommand)]
enum DisplayAction {
    /// Change display settings.
    Set {
        /// Backlight brightness, 0 to 100.
        #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
        brightness: Option<u8>,
    },
}

#[derive(Subcommand)]
enum MediaAction {
    /// List the media files stored on the display and its free space.
    Ls,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let tty = cli.tty.as_deref();
    let result: CommandResult = match cli.command {
        Command::Doctor => doctor::run(cli.json).map_err(Failure::from),
        Command::Devices => devices::run(cli.json).map_err(Failure::from),
        Command::Info => info::run(cli.json, tty),
        Command::Display {
            action: DisplayAction::Set { brightness },
        } => display::set(cli.json, tty, brightness),
        Command::Media {
            action: MediaAction::Ls,
        } => media::ls(cli.json, tty),
    };
    match result {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            failure.code
        }
    }
}
