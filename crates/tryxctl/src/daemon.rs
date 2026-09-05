//! The long-running process that owns the display: it keeps the panel
//! awake (sysinfo pushes on the legacy firmware, pings and the overlay lease
//! on KANALI), restores the saved screen on start, and serves every other
//! command over the socket in [`crate::ipc`].

use crate::exit::{self, CommandResult, Failure};
use crate::ipc::{self, DaemonStatus, Reply, Request};
use crate::legacy::{Backend, Info, Protocol};
use crate::metrics::{pc_info, require_linux};
use crate::{kanali, legacy, state};
use serde_json::json;
use std::os::unix::net::UnixListener;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tryx_legacy::Client;
use tryx_monitor::Monitor;

pub const SERVICE_NAME: &str = "tryxctl.service";

type Envelope = (Request, Sender<Reply>);

/// The display the daemon holds open.
enum Owned {
    Legacy(Client),
    Kanali(Box<kanali::Link>),
}

/// How often to look for the display while it is gone.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(3);
/// Silent pushes in a row before the link is presumed dead.
const SILENT_PUSH_LIMIT: u32 = 3;

/// Whether an error means the link itself is gone, rather than the device
/// declining one command.
fn legacy_lost(error: &tryx_legacy::LegacyError) -> bool {
    use tryx_legacy::LegacyError;
    matches!(error, LegacyError::Io(_) | LegacyError::Serial { .. })
}

fn kanali_lost(error: &tryx_kanali::KanaliError) -> bool {
    use tryx_kanali::KanaliError;
    use tryx_kanali::transport::TransportError;
    matches!(
        error,
        KanaliError::Transport(
            TransportError::Disconnected | TransportError::Usb(_) | TransportError::Io(_)
        )
    )
}

/// Finds the display and opens it, filling in the identity fields.
fn open_backend(session: &legacy::Session, status: &mut DaemonStatus) -> Result<Owned, Failure> {
    match session.select_backend()? {
        Backend::Legacy(target) => {
            let mut client = session.open(&target)?;
            status.protocol = Protocol::Legacy;
            status.product = None;
            status.tty = target.tty.clone();
            let info = client
                .handshake()
                .map_err(|error| Failure::device(format!("handshake: {error}")))?;
            status.info = Some(Info::Legacy(info));
            Ok(Owned::Legacy(client))
        }
        Backend::Kanali { id, product } => {
            let link = kanali::open(Some(&id), session.verbose)?;
            status.protocol = Protocol::Kanali;
            status.product = Some(product);
            status.tty = id;
            status.info = link.info.clone().map(Info::Kanali);
            Ok(Owned::Kanali(Box::new(link)))
        }
    }
}

fn push_interval(owned: &Owned, interval: u64) -> Duration {
    // The KANALI overlay wants values about every second; the serial link
    // is happier with the slower cadence the user picked.
    match owned {
        Owned::Legacy(_) => Duration::from_secs(interval.max(1)),
        Owned::Kanali(_) => Duration::from_secs(interval.clamp(1, 2)),
    }
}

pub fn run(session: &legacy::Session, interval: u64, quiet: bool) -> CommandResult {
    if !Monitor::supported() {
        return Err(Failure::environment(
            "the daemon reads /proc and /sys; Linux only",
        ));
    }
    if ipc::available() {
        return Err(Failure::usage(
            "a tryxctl daemon is already running on this socket",
        ));
    }
    let mut status = DaemonStatus {
        started_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        interval,
        ..DaemonStatus::default()
    };
    // The display may not be there yet (service started before the USB
    // device enumerated); keep looking rather than failing.
    let mut owned = match open_backend(session, &mut status) {
        Ok(mut owned) => {
            restore(&mut owned, &mut status, quiet);
            status.connected = true;
            Some(owned)
        }
        Err(failure) => {
            if !quiet {
                eprintln!("not connected: {}; retrying", failure.message);
            }
            status.last_error = Some(failure.message);
            None
        }
    };

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
    let mut next_keepalive = Instant::now();
    let mut next_reconnect = Instant::now() + RECONNECT_INTERVAL;
    let mut silent_pushes = 0u32;
    loop {
        let next = match &owned {
            Some(Owned::Legacy(_)) => next_push,
            Some(Owned::Kanali(_)) => next_push.min(next_keepalive),
            None => next_reconnect,
        };
        match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok((request, reply_tx)) => {
                let (reply, lost) = handle(&mut owned, &mut status, request);
                let _ = reply_tx.send(reply);
                if lost {
                    disconnect(&mut owned, &mut status, quiet);
                    next_reconnect = Instant::now() + RECONNECT_INTERVAL;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                match owned.as_mut() {
                    None => {
                        if now >= next_reconnect {
                            match open_backend(session, &mut status) {
                                Ok(mut reopened) => {
                                    status.reconnects += 1;
                                    status.last_error = None;
                                    if !quiet {
                                        println!("reconnected to {}", status.tty);
                                    }
                                    restore(&mut reopened, &mut status, quiet);
                                    status.connected = true;
                                    owned = Some(reopened);
                                    silent_pushes = 0;
                                    next_push = now;
                                    next_keepalive = now;
                                }
                                Err(failure) => {
                                    status.last_error = Some(failure.message);
                                    next_reconnect = now + RECONNECT_INTERVAL;
                                }
                            }
                        }
                    }
                    Some(current) => {
                        let mut lost = false;
                        if let Owned::Kanali(link) = current
                            && now >= next_keepalive
                        {
                            if let Err(error) = link.keepalive() {
                                lost |= kanali_lost(&error);
                                status.last_error = Some(format!("keepalive: {error}"));
                            }
                            next_keepalive = now + tryx_kanali::KEEPALIVE_INTERVAL;
                        }
                        if now >= next_push {
                            match push(current, &mut status, &mut monitor, quiet) {
                                Ok(()) => silent_pushes = 0,
                                Err(dead) => {
                                    silent_pushes += 1;
                                    lost |= dead || silent_pushes >= SILENT_PUSH_LIMIT;
                                }
                            }
                            next_push = now + push_interval(current, interval);
                        }
                        if lost {
                            disconnect(&mut owned, &mut status, quiet);
                            next_reconnect = now + RECONNECT_INTERVAL;
                            silent_pushes = 0;
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(exit::ok())
}

/// Drops the link so the next tick looks for the display again.
fn disconnect(owned: &mut Option<Owned>, status: &mut DaemonStatus, quiet: bool) {
    if owned.take().is_some() {
        status.connected = false;
        if !quiet {
            eprintln!(
                "lost the display ({}); retrying every {} s",
                status.last_error.as_deref().unwrap_or("no error recorded"),
                RECONNECT_INTERVAL.as_secs()
            );
        }
    }
}

fn restore(owned: &mut Owned, status: &mut DaemonStatus, quiet: bool) {
    let mut saved = state::load();
    match owned {
        Owned::Legacy(client) => {
            if !saved.screen.media.is_empty() || !saved.screen.preset_id.is_empty() {
                match legacy::apply_screen(client, &mut saved) {
                    Ok(_) => {
                        if !quiet {
                            println!("restored {}", describe_screen(&saved.screen));
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
        }
        Owned::Kanali(link) => {
            // The device keeps its own media selection; only push what the
            // host was asked to show.
            let wants_overlay = !saved.screen.sysinfo_display.is_empty()
                || !saved.screen.settings.badges.is_empty();
            if !saved.screen.media.is_empty() || !saved.screen.preset_id.is_empty() || wants_overlay
            {
                match link.apply_state(&saved) {
                    Ok(_) => {
                        if !quiet {
                            println!("restored {}", describe_screen(&saved.screen));
                        }
                    }
                    Err(failure) => {
                        status.last_error = Some(format!("restore: {}", failure.message))
                    }
                }
            } else if let Err(failure) = link.adopt_state(&saved) {
                status.last_error = Some(format!("restore: {}", failure.message));
            }
        }
    }
    let _ = state::save(&saved);
    status.screen = saved.screen;
}

fn describe_screen(screen: &tryx_legacy::ScreenConfig) -> String {
    let media = if !screen.preset_id.is_empty() {
        screen.preset_id.clone()
    } else if screen.media.is_empty() {
        "the device's media".to_string()
    } else {
        screen.media.join(", ")
    };
    if screen.sysinfo_display.is_empty() {
        media
    } else {
        format!("{media} with {}", screen.sysinfo_display.join(", "))
    }
}

/// One metrics push. `Err(true)` means the link is gone.
fn push(
    owned: &mut Owned,
    status: &mut DaemonStatus,
    monitor: &mut Monitor,
    quiet: bool,
) -> Result<(), bool> {
    let mut sample = monitor.sample();
    let result = match owned {
        Owned::Legacy(client) => {
            sample.timestamp_ms += tryx_legacy::local_utc_offset_ms();
            client
                .send_sysinfo(&pc_info(&sample))
                .map(|fans| status.fans = fans)
                .map_err(|error| (error.to_string(), legacy_lost(&error)))
        }
        Owned::Kanali(link) => link
            .push(&sample)
            .map_err(|error| (error.to_string(), kanali_lost(&error))),
    };
    status.sample = Some(sample);
    match result {
        Ok(()) => {
            status.pushes += 1;
            status.last_error = None;
            Ok(())
        }
        Err((message, lost)) => {
            status.last_error = Some(format!("push: {message}"));
            if !quiet {
                eprintln!("push failed: {message}");
            }
            Err(lost)
        }
    }
}

/// Answers one request. The flag says the link was found dead.
fn handle(owned: &mut Option<Owned>, status: &mut DaemonStatus, request: Request) -> (Reply, bool) {
    if let Request::Status = request {
        return (Reply::ok(&*status), false);
    }
    let Some(owned) = owned.as_mut() else {
        return (
            Reply::error(format!(
                "the display is disconnected; the daemon retries every {} s",
                RECONNECT_INTERVAL.as_secs()
            )),
            false,
        );
    };
    let legacy_error = |error: tryx_legacy::LegacyError| (error.to_string(), legacy_lost(&error));
    let kanali_error = |error: tryx_kanali::KanaliError| (error.to_string(), kanali_lost(&error));
    let result: Result<Reply, (String, bool)> = match (owned, request) {
        (_, Request::Status) => unreachable!("answered above"),
        (Owned::Legacy(client), Request::Info) => client
            .handshake()
            .map(|info| {
                status.info = Some(Info::Legacy(info.clone()));
                Reply::ok(Info::Legacy(info))
            })
            .map_err(legacy_error),
        (Owned::Kanali(link), Request::Info) => link
            .info
            .clone()
            .map(|info| Reply::ok(Info::Kanali(info)))
            .ok_or_else(|| {
                (
                    "this product reports no device information".to_string(),
                    false,
                )
            }),
        (Owned::Legacy(client), Request::Apply { state }) => {
            let mut state = *state;
            legacy::apply_screen(client, &mut state)
                .map(|response| {
                    status.screen = state.screen.clone();
                    let _ = crate::state::save(&state);
                    Reply::ok(json!({"status": response.status}))
                })
                .map_err(legacy_error)
        }
        (Owned::Kanali(link), Request::Apply { state }) => link
            .apply_state(&state)
            .map(|word| {
                status.screen = state.screen.clone();
                let _ = crate::state::save(&state);
                Reply::ok(json!({"status": word}))
            })
            .map_err(|f| (f.message, false)),
        (Owned::Legacy(client), Request::Brightness { value }) => client
            .set_brightness(value)
            .map(|r| Reply::ok(json!({"status": r.status})))
            .map_err(legacy_error),
        (Owned::Kanali(link), Request::Brightness { value }) => link
            .device
            .set_brightness(u32::from(value))
            .map(|_| Reply::ok(json!({"status": "applied"})))
            .map_err(kanali_error),
        (Owned::Legacy(client), Request::Rotate { degrees }) => client
            .set_rotation(degrees)
            .map(|r| Reply::ok(json!({"status": r.status})))
            .map_err(legacy_error),
        (Owned::Kanali(link), Request::Rotate { degrees }) => {
            let change = tryx_kanali::Change {
                rotation: Some(u32::from(degrees)),
                ..Default::default()
            };
            link.device
                .apply(&change, link.overlay.as_ref())
                .map(|_| Reply::ok(json!({"status": "applied"})))
                .map_err(kanali_error)
        }
        (Owned::Legacy(_), Request::Readback) => Ok(Reply::ok(legacy::Readback::last_applied(
            &state::load(),
            status.info.clone(),
            status.fans.clone(),
        ))),
        (Owned::Kanali(link), Request::Readback) => link
            .device
            .display_state()
            .map(|device_state| {
                Reply::ok(legacy::Readback::from_kanali(
                    &device_state,
                    &state::load(),
                    link.info.clone().map(Info::Kanali),
                ))
            })
            .map_err(kanali_error),
        (Owned::Legacy(client), Request::DeleteMedia { names }) => client
            .delete_media(&names)
            .map(|r| Reply::ok(json!({"status": r.status})))
            .map_err(legacy_error),
        (Owned::Kanali(link), Request::DeleteMedia { names }) => names
            .iter()
            .try_for_each(|name| link.device.delete(name))
            .map(|()| Reply::ok(json!({"status": "applied"})))
            .map_err(kanali_error),
        (Owned::Legacy(client), Request::FanLcd { percent }) => client
            .set_fan_lcd(percent)
            .map(|r| Reply::ok(json!({"status": r.status})))
            .map_err(legacy_error),
        // The panel goes away to reboot; drop the link now rather than
        // waiting for the pushes to fail.
        (Owned::Legacy(client), Request::Reboot) => client
            .reboot()
            .map(|r| Reply::ok(json!({"status": r.status})))
            .map_err(legacy_error)
            .and_then(|reply| Err((format!("rebooting ({})", reply.value["status"]), true))),
        (
            Owned::Legacy(client),
            Request::Raw {
                method,
                command,
                body,
                wait,
            },
        ) => {
            if wait {
                client
                    .link_mut()
                    .request_with(&method, &command, &body)
                    .map(|r| Reply::ok(json!({"status": r.status, "body": r.body})))
                    .map_err(legacy_error)
            } else {
                client
                    .link_mut()
                    .send_with(&method, &command, &body)
                    .map(|()| Reply::ok(json!({"sent": true})))
                    .map_err(legacy_error)
            }
        }
        (Owned::Kanali(link), Request::Catalog) => {
            link.device.catalog().map(Reply::ok).map_err(kanali_error)
        }
        (Owned::Kanali(link), Request::Upload { path, name }) => link
            .device
            .upload(&path, &name, |_, _| {})
            .map(|()| Reply::ok(json!({"uploaded": name})))
            .map_err(kanali_error),
        (Owned::Kanali(_), Request::FanLcd { .. } | Request::Reboot | Request::Raw { .. }) => {
            Err(("not available on the KANALI firmware".to_string(), false))
        }
        (Owned::Legacy(_), Request::Catalog | Request::Upload { .. }) => Err((
            "the legacy firmware manages media over adb".to_string(),
            false,
        )),
    };
    match result {
        Ok(reply) => (reply, false),
        // A reboot is reported as success to the caller.
        Err((message, true)) if message.starts_with("rebooting") => {
            status.last_error = Some(message.clone());
            (Reply::ok(json!({"status": "200", "rebooting": true})), true)
        }
        Err((message, lost)) => {
            status.last_error = Some(message.clone());
            (Reply::error(message), lost)
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

/// Writes and starts a systemd user service running `tryxctl daemon`.
pub fn install(
    json: bool,
    interval: u64,
    tty: Option<&str>,
    device: Option<&str>,
) -> CommandResult {
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
    let tty_arg = tty
        .map(|t| format!(" --tty {t}"))
        .or_else(|| device.map(|d| format!(" --device {d}")))
        .unwrap_or_default();
    let text = format!(
        "[Unit]\nDescription=TRYX display daemon (keepalive, metrics, commands)\nDocumentation=https://github.com/nicklambourne/tryxctl\n\n[Service]\nExecStart={} daemon --interval {interval} --quiet{tty_arg}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
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

/// `tryxctl daemon status`: what the running daemon knows.
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
    let has_pump = status.info.as_ref().map(Info::has_pump);
    let fans = match (status.fans.lcd_fan_rpm, status.fans.pump_rpm, has_pump) {
        (Some(fan), Some(pump), _) => format!("LCD fan {fan} rpm, pump {pump} rpm"),
        (Some(fan), None, Some(false)) => {
            format!("LCD fan {fan} rpm; no pump tachometer on this model")
        }
        (Some(fan), None, _) => format!("LCD fan {fan} rpm"),
        (None, Some(pump), _) => format!("pump {pump} rpm"),
        (None, None, _) => "not reported".to_string(),
    };
    let storage = status
        .fans
        .available_storage
        .map(crate::output::human_bytes)
        .unwrap_or_else(|| "unknown".to_string());
    let device = status
        .info
        .as_ref()
        .map(Info::summary)
        .unwrap_or_else(|| "not identified".into());
    print!(
        "{}",
        crate::output::key_values(&[
            ("Device", device),
            ("Protocol", status.protocol.label().to_string()),
            (
                "Link",
                if status.connected {
                    status.tty.clone()
                } else {
                    format!("{} (disconnected, retrying)", status.tty)
                },
            ),
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
            ("Free storage", storage),
            ("Reconnects", status.reconnects.to_string()),
            (
                "Last error",
                status.last_error.clone().unwrap_or_else(|| "none".into())
            ),
        ])
    );
    Ok(exit::ok())
}
