mod devices;
mod display;
mod doctor;
mod exit;
mod info;
mod legacy;
mod media;
mod output;
mod show;

use clap::{Parser, Subcommand};
use exit::{CommandResult, Failure};
use std::path::PathBuf;
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

    /// Dump every frame exchanged with the display to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

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
    /// Play media already stored on the display.
    Show {
        /// File names as listed by `tryx media ls`.
        #[arg(required = true, value_name = "NAME")]
        media: Vec<String>,
        /// Playback mode.
        #[arg(long, value_parser = ["Single", "Loop", "Shuffle"], default_value = "Single", value_name = "MODE")]
        play: String,
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
    /// Validate files against the display without touching it.
    Check {
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Treat anything that would be changed automatically as an error.
        #[arg(long)]
        strict: bool,
        /// File name to use on the display.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[command(flatten)]
        transform: media::TransformArgs,
    },
    /// Convert a file to what the display expects, without uploading it.
    Convert {
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Where to write the result; defaults to the display file name.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Show the plan and the ffmpeg command without running it.
        #[arg(long)]
        dry_run: bool,
        /// File name to use on the display.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[command(flatten)]
        transform: media::TransformArgs,
    },
    /// Validate, convert if needed, and copy a file to the display.
    Upload {
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// File name to use on the display.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Start playing it once uploaded.
        #[arg(long)]
        show: bool,
        /// Overwrite a file of the same name.
        #[arg(long)]
        replace: bool,
        /// Show the plan without converting or uploading.
        #[arg(long)]
        dry_run: bool,
        /// Refuse files that would be changed automatically.
        #[arg(long)]
        strict: bool,
        #[command(flatten)]
        transform: media::TransformArgs,
    },
    /// Delete media files from the display.
    Rm {
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let session = legacy::Session {
        tty: cli.tty.clone(),
        verbose: cli.verbose,
    };
    let result: CommandResult = match cli.command {
        Command::Doctor => doctor::run(cli.json).map_err(Failure::from),
        Command::Devices => devices::run(cli.json).map_err(Failure::from),
        Command::Info => info::run(cli.json, &session),
        Command::Display {
            action: DisplayAction::Set { brightness },
        } => display::set(cli.json, &session, brightness),
        Command::Media { action } => match action {
            MediaAction::Ls => media::ls(cli.json, &session),
            MediaAction::Check {
                files,
                strict,
                name,
                transform,
            } => media::check(cli.json, &files, strict, &transform, name),
            MediaAction::Convert {
                file,
                output,
                dry_run,
                name,
                transform,
            } => media::convert(cli.json, &file, output, dry_run, &transform, name),
            MediaAction::Upload {
                file,
                name,
                show,
                replace,
                dry_run,
                strict,
                transform,
            } => media::upload(
                cli.json, &session, &file, name, show, replace, dry_run, strict, &transform,
            ),
            MediaAction::Rm { names } => media::rm(cli.json, &session, &names),
        },
        Command::Show { media, play } => show::run(cli.json, &session, &media, &play),
    };
    match result {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            failure.code
        }
    }
}
