//! The long-running process that owns the serial port: it keeps the panel
//! awake with sysinfo pushes, restores the saved screen on start, and serves
//! every other command over the socket in [`crate::ipc`].

use crate::exit::{self, CommandResult, Failure};
use crate::ipc::{self, DaemonStatus, Reply, Request};
use crate::metrics::{pc_info, require_linux};
use crate::{legacy, state};
use serde_json::json;
use std::os::unix::net::UnixListener;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tryx_legacy::Client;
use tryx_monitor::Monitor;

pub const SERVICE_NAME: &str = "tryx-metrics.service";

type Envelope = (Request, Sender<Reply>);

pub fn run(session: &legacy::Session, interval: u64, quiet: bool) -> CommandResult {
    if !Monitor::supported() {
        return Err(Failure::environment(
            "the daemon reads /proc and /sys; Linux only",
        ));
    }
    if ipc::available() {
        return Err(Failure::usage(
            "a tryx daemon is already running on this socket",
        ));
    }
    let target = session.select_direct()?;
    let mut client = session.open(&target)?;

    let mut status = DaemonStatus {
        tty: target.tty.clone(),
        started_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        interval,
        ..DaemonStatus::default()
    };
    match client.handshake() {
        Ok(info) => status.info = Some(info),
        Err(error) => status.last_error = Some(format!("handshake: {error}")),
    }
    restore(&mut client, &mut status, quiet);

    let socket = ipc::socket_path();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .map_err(|e| Failure::environment(format!("cannot listen on {}: {e}", socket.display())))?;
    let (tx, rx) = mpsc::channel::<Envelope>();
    std::thread::spawn(move || serve(listener, tx));
    if !quiet {
        println!("listening on {}", socket.display());
    }

    let mut monitor = Monitor::new();
    let mut next_push = Instant::now();
    loop {
        let wait = next_push.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok((request, reply_tx)) => {
                let reply = handle(&mut client, &mut status, &mut monitor, request);
                let _ = reply_tx.send(reply);
            }
            Err(RecvTimeoutError::Timeout) => {
                push(&mut client, &mut status, &mut monitor, quiet);
                next_push = Instant::now() + Duration::from_secs(interval.max(1));
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(exit::ok())
}

fn restore(client: &mut Client, status: &mut DaemonStatus, quiet: bool) {
    let mut saved = state::load();
    if !saved.screen.media.is_empty() {
        match legacy::apply_screen(client, &mut saved) {
            Ok(_) => {
                if !quiet {
                    println!("restored {}", saved.screen.media.join(", "));
                }
            }
            Err(error) => status.last_error = Some(format!("restore: {error}")),
        }
    }
    if let Some(percent) = saved.fan_lcd_percent
        && let Err(error) = client.set_fan_lcd(percent)
    {
        status.last_error = Some(format!("fan speed: {error}"));
    }
    let _ = state::save(&saved);
    status.screen = saved.screen;
}

fn push(client: &mut Client, status: &mut DaemonStatus, monitor: &mut Monitor, quiet: bool) {
    let mut sample = monitor.sample();
    sample.timestamp_ms += tryx_legacy::local_utc_offset_ms();
    match client.send_sysinfo(&pc_info(&sample)) {
        Ok(fans) => {
            status.fans = fans;
            status.pushes += 1;
            status.last_error = None;
        }
        Err(error) => {
            status.last_error = Some(format!("push: {error}"));
            if !quiet {
                eprintln!("push failed: {error}");
            }
        }
    }
    status.sample = Some(sample);
}

fn handle(
    client: &mut Client,
    status: &mut DaemonStatus,
    monitor: &mut Monitor,
    request: Request,
) -> Reply {
    let result: Result<Reply, String> = match request {
        Request::Status => Ok(Reply::ok(&*status)),
        Request::Info => client
            .handshake()
            .map(Reply::ok)
            .map_err(|e| e.to_string())
            .inspect(|reply| {
                status.info = serde_json::from_value(reply.value.clone()).ok();
            }),
        Request::Apply { state } => {
            let mut state = *state;
            legacy::apply_screen(client, &mut state)
                .map(|response| {
                    status.screen = state.screen.clone();
                    let _ = crate::state::save(&state);
                    Reply::ok(serde_json::json!({"status": response.status}))
                })
                .map_err(|e| e.to_string())
        }
        Request::Brightness { value } => client
            .set_brightness(value)
            .map(|r| Reply::ok(serde_json::json!({"status": r.status})))
            .map_err(|e| e.to_string()),
        Request::DeleteMedia { names } => client
            .delete_media(&names)
            .map(|r| Reply::ok(serde_json::json!({"status": r.status})))
            .map_err(|e| e.to_string()),
        Request::FanLcd { percent } => client
            .set_fan_lcd(percent)
            .map(|r| Reply::ok(serde_json::json!({"status": r.status})))
            .map_err(|e| e.to_string()),
        Request::Reboot => client
            .reboot()
            .map(|r| Reply::ok(serde_json::json!({"status": r.status})))
            .map_err(|e| e.to_string()),
        Request::Raw {
            command,
            body,
            wait,
        } => {
            if wait {
                client
                    .link_mut()
                    .request(&command, &body)
                    .map(|r| Reply::ok(serde_json::json!({"status": r.status, "body": r.body})))
                    .map_err(|e| e.to_string())
            } else {
                client
                    .link_mut()
                    .send(&command, &body)
                    .map(|()| Reply::ok(serde_json::json!({"sent": true})))
                    .map_err(|e| e.to_string())
            }
        }
    };
    // Any command may have moved the panel; take a fresh sample soon after.
    let _ = monitor;
    match result {
        Ok(reply) => reply,
        Err(message) => {
            status.last_error = Some(message.clone());
            Reply::error(message)
        }
    }
}

fn serve(listener: UnixListener, tx: Sender<Envelope>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Ok(bytes) = ipc::read_frame(&mut stream) else {
            continue;
        };
        let reply = match serde_json::from_slice::<Request>(&bytes) {
            Ok(request) => {
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx.send((request, reply_tx)).is_err() {
                    return;
                }
                reply_rx
                    .recv()
                    .unwrap_or_else(|_| Reply::error("daemon stopped"))
            }
            Err(error) => Reply::error(format!("bad request: {error}")),
        };
        if let Ok(bytes) = serde_json::to_vec(&reply) {
            let _ = ipc::write_frame(&mut stream, &bytes);
        }
    }
}

fn user_unit_path() -> Result<std::path::PathBuf, Failure> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })
        .ok_or_else(|| Failure::environment("HOME is not set"))?;
    Ok(base.join("systemd/user").join(SERVICE_NAME))
}

fn systemctl(args: &[&str]) -> Result<String, Failure> {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| Failure::environment(format!("systemctl: {error}")))?;
    if !output.status.success() {
        return Err(Failure::environment(format!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Writes and starts a systemd user service running `tryx daemon`.
pub fn install(json: bool, interval: u64, tty: Option<&str>) -> CommandResult {
    require_linux()?;
    let binary = std::env::current_exe()
        .map_err(|error| Failure::environment(format!("cannot locate this binary: {error}")))?;
    if binary.components().any(|c| c.as_os_str() == "target") {
        eprintln!(
            "warning: the service will run {}, a development build; install a release binary and rerun",
            binary.display()
        );
    }
    let unit = user_unit_path()?;
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tty_arg = tty.map(|t| format!(" --tty {t}")).unwrap_or_default();
    let text = format!(
        "[Unit]\nDescription=TRYX display daemon (keepalive, metrics, commands)\nDocumentation=https://github.com/nicklambourne/tryx-cli\n\n[Service]\nExecStart={} daemon --interval {interval} --quiet{tty_arg}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        binary.display()
    );
    std::fs::write(&unit, text)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", SERVICE_NAME])?;
    let linger = std::process::Command::new("loginctl")
        .arg("enable-linger")
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "unit": unit,
                "binary": binary,
                "interval": interval,
                "linger": linger,
            }))?
        );
    } else {
        println!(
            "installed and started {} ({})",
            SERVICE_NAME,
            unit.display()
        );
        println!(
            "{}",
            if linger {
                "lingering enabled: the service also runs while you are logged out"
            } else {
                "could not enable lingering; run `loginctl enable-linger` so it survives logout"
            }
        );
        println!("check it with: systemctl --user status {SERVICE_NAME}");
    }
    Ok(exit::ok())
}

pub fn uninstall(json: bool) -> CommandResult {
    require_linux()?;
    let unit = user_unit_path()?;
    let _ = systemctl(&["disable", "--now", SERVICE_NAME]);
    let removed = std::fs::remove_file(&unit).is_ok();
    let _ = systemctl(&["daemon-reload"]);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"unit": unit, "removed": removed}))?
        );
    } else {
        println!(
            "{} {}",
            if removed { "removed" } else { "no unit at" },
            unit.display()
        );
    }
    Ok(exit::ok())
}

/// `tryx daemon status`: what the running daemon knows.
pub fn status(json: bool) -> CommandResult {
    let Some(reply) = ipc::call(&Request::Status)? else {
        return Err(Failure::device(format!(
            "no daemon is listening on {}",
            ipc::socket_path().display()
        )));
    };
    if !reply.ok {
        return Err(Failure::device(reply.error.unwrap_or_default()));
    }
    let status: DaemonStatus = serde_json::from_value(reply.value)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(exit::ok());
    }
    let uptime = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        - status.started_unix;
    let fans = match (status.fans.lcd_fan_rpm, status.fans.pump_rpm) {
        (Some(fan), Some(pump)) => format!("LCD fan {fan} rpm, pump {pump} rpm"),
        (Some(fan), None) => format!("LCD fan {fan} rpm"),
        (None, Some(pump)) => format!("pump {pump} rpm"),
        (None, None) => "not reported".to_string(),
    };
    let device = status
        .info
        .as_ref()
        .map(|i| {
            format!(
                "{} firmware {} serial {}",
                i.product_id, i.firmware, i.serial
            )
        })
        .unwrap_or_else(|| "not identified".into());
    print!(
        "{}",
        crate::output::key_values(&[
            ("Device", device),
            ("Port", status.tty.clone()),
            (
                "Uptime",
                format!(
                    "{uptime} s, {} pushes every {} s",
                    status.pushes, status.interval
                )
            ),
            (
                "Showing",
                if status.screen.media.is_empty() {
                    "nothing".into()
                } else {
                    status.screen.media.join(", ")
                }
            ),
            (
                "Overlay",
                if status.screen.sysinfo_display.is_empty() {
                    "off".into()
                } else {
                    status.screen.sysinfo_display.join(", ")
                }
            ),
            ("Fans", fans),
            (
                "Last error",
                status.last_error.clone().unwrap_or_else(|| "none".into())
            ),
        ])
    );
    Ok(exit::ok())
}
