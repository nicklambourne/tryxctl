mod devices;
mod doctor;
mod exit;
mod output;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

/// Control TRYX cooler displays from the terminal.
#[derive(Parser)]
#[command(name = "tryx", version, about)]
struct Cli {
    /// Emit machine-readable JSON instead of human output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check ffmpeg, USB permissions, and connected displays.
    Doctor,
    /// List connected TRYX displays.
    Devices,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Doctor => doctor::run(cli.json),
        Command::Devices => devices::run(cli.json),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            exit::environment()
        }
    }
}
