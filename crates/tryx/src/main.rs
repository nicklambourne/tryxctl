mod daemon;
mod devices;
mod display;
mod doctor;
mod exit;
mod info;
mod ipc;
mod kanali;
mod legacy;
mod media;
mod metrics;
mod output;
mod raw;
mod show;
mod state;
mod tui;

use clap::{CommandFactory, Parser, Subcommand};
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

    /// USB id of a KANALI-firmware display (see `tryx devices`), bypassing discovery.
    #[arg(long, global = true, value_name = "ID", conflicts_with = "tty")]
    device: Option<String>,

    /// Dump every frame exchanged with the display to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Open the display directly even when the daemon is running.
    #[arg(long, global = true)]
    direct: bool,

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
    /// Show live host metrics on the display.
    Metrics {
        #[command(subcommand)]
        action: MetricsAction,
    },
    /// Read the fan tachometers, or set the display-block fan speed.
    Fans {
        /// Poll every N seconds until interrupted.
        #[arg(long, value_name = "SECONDS")]
        watch: Option<u64>,
        /// Fixed speed for the display-block fan, 0 to 100.
        #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
        lcd_speed: Option<u8>,
    },
    /// Run the daemon that owns the port: keepalive, metrics, and commands.
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
        /// Seconds between metric pushes.
        #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=60))]
        interval: u64,
        /// Log nothing but errors.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Open the interactive interface.
    Tui,
    /// Print a shell completion script.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the manual page in roff.
    #[command(hide = true)]
    Manpage,
    /// Send one raw legacy command with a JSON body (protocol exploration).
    #[command(hide = true)]
    Raw {
        /// Command name, e.g. brightness.
        command: String,
        /// JSON body; empty when omitted.
        #[arg(default_value = "")]
        body: String,
        /// Do not wait for a reply.
        #[arg(long)]
        no_wait: bool,
        /// Repeat every N seconds until interrupted.
        #[arg(long, value_name = "SECONDS")]
        every: Option<u64>,
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
    /// Change brightness, the filter effect, or the sleep behaviour.
    Set {
        #[command(flatten)]
        args: display::SetArgs,
    },
    /// Reboot the display.
    Reboot,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// What the running daemon knows.
    Status,
    /// Install and start a systemd user service running the daemon.
    Install {
        /// Seconds between metric pushes.
        #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=60))]
        interval: u64,
    },
    /// Stop and remove the service.
    Uninstall,
}

// Command enums are built once; the size difference between variants
// does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum MetricsAction {
    /// Print what this host can measure.
    Status,
    /// Configure the overlay: which metrics, where, and in what colour.
    Set {
        #[command(flatten)]
        args: metrics::SetArgs,
    },
    /// Send host metrics to the display, repeatedly until interrupted.
    Push {
        /// Seconds between samples.
        #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=60))]
        interval: u64,
        /// Send one sample and exit.
        #[arg(long)]
        once: bool,
        /// Do not print each sample.
        #[arg(short, long)]
        quiet: bool,
        /// Do not restore the saved screen before pushing.
        #[arg(long)]
        no_apply: bool,
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
    /// Render a frame exactly as the display would get it.
    Preview {
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Position in the video to preview, in seconds.
        #[arg(long, value_name = "SECONDS")]
        at: Option<f64>,
        /// A 3×3 sheet of frames spread over the whole video.
        #[arg(long)]
        sheet: bool,
        /// Write a PNG here instead of showing it in the terminal.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
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
        device: cli.device.clone(),
        verbose: cli.verbose,
        direct: cli.direct,
    };
    let result: CommandResult = match cli.command {
        Command::Doctor => doctor::run(cli.json).map_err(Failure::from),
        Command::Devices => devices::run(cli.json).map_err(Failure::from),
        Command::Info => info::run(cli.json, &session),
        Command::Display { action } => match action {
            DisplayAction::Set { args } => display::set(cli.json, &session, &args),
            DisplayAction::Reboot => display::reboot(cli.json, &session),
        },
        Command::Fans { watch, lcd_speed } => metrics::fans(cli.json, &session, watch, lcd_speed),
        Command::Daemon {
            action,
            interval,
            quiet,
        } => match action {
            None => daemon::run(&session, interval, quiet),
            Some(DaemonAction::Status) => daemon::status(cli.json),
            Some(DaemonAction::Install { interval }) => daemon::install(
                cli.json,
                interval,
                session.tty.as_deref(),
                session.device.as_deref(),
            ),
            Some(DaemonAction::Uninstall) => daemon::uninstall(cli.json),
        },
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
            MediaAction::Preview {
                file,
                at,
                sheet,
                output,
                transform,
            } => media::preview(cli.json, &file, at, sheet, output, &transform),
            MediaAction::Rm { names } => media::rm(cli.json, &session, &names),
        },
        Command::Show { media, play } => show::run(cli.json, &session, &media, &play),
        Command::Tui => tui::run(&session),
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "tryx", &mut std::io::stdout());
            Ok(exit::ok())
        }
        Command::Raw {
            command,
            body,
            no_wait,
            every,
        } => raw::run(&session, &command, &body, no_wait, every),
        Command::Manpage => {
            let mut out = Vec::new();
            match clap_mangen::Man::new(Cli::command()).render(&mut out) {
                Ok(()) => {
                    use std::io::Write;
                    std::io::stdout().write_all(&out).ok();
                    Ok(exit::ok())
                }
                Err(error) => Err(Failure::environment(error.to_string())),
            }
        }
        Command::Metrics { action } => match action {
            MetricsAction::Status => metrics::status(cli.json),
            MetricsAction::Set { args } => metrics::set(cli.json, &session, &args),
            MetricsAction::Push {
                interval,
                once,
                quiet,
                no_apply,
            } => metrics::push(cli.json, &session, interval, once, quiet, !no_apply),
        },
    };
    match result {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            failure.code
        }
    }
}
