use crate::exit::{self, CommandResult, Failure};
use crate::{legacy, output};
use serde_json::json;
use tryx_legacy::adb::{self, Adb};

pub fn ls(json: bool, session: &legacy::Session) -> CommandResult {
    let target = session.select()?;
    let adb = Adb::new()?;
    let devices = adb.devices()?;
    let selected = adb::select(&devices, target.usb_serial(), target.sysfs_name()).ok_or_else(|| {
        Failure::device(if devices.is_empty() {
            "adb sees no devices; install packaging/udev/71-tryx-legacy.rules and replug the display"
        } else {
            "adb sees devices, but none matches the display's serial or USB port"
        })
    })?;
    if selected.state != "device" {
        return Err(Failure::device(format!(
            "adb reports the display ({}) as {}",
            selected.serial, selected.state
        )));
    }
    let adb = adb.with_serial(selected.serial.clone());
    let files = adb.list_media()?;
    let storage = adb.free_space()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "adb_serial": selected.serial,
                "directory": adb::MEDIA_DIR,
                "files": files,
                "storage": storage,
            }))?
        );
        return Ok(exit::ok());
    }
    if files.is_empty() {
        println!("No user media on the display.");
    } else {
        let rows: Vec<Vec<String>> = files
            .iter()
            .map(|file| vec![file.name.clone(), output::human_bytes(file.size)])
            .collect();
        print!("{}", output::table(&["NAME", "SIZE"], &rows));
    }
    let used: u64 = files.iter().map(|file| file.size).sum();
    println!(
        "{} file(s), {} in {}; {} free of {} on the display",
        files.len(),
        output::human_bytes(used),
        adb::MEDIA_DIR,
        output::human_bytes(storage.available_kib * 1024),
        output::human_bytes(storage.total_kib * 1024),
    );
    Ok(exit::ok())
}
