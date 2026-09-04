use crate::{exit, output};
use anyhow::Context;
use std::process::ExitCode;
use tryx_device::discovery::Discovery;

pub fn run(json: bool) -> anyhow::Result<ExitCode> {
    let discovery = tryx_device::discover().context("could not enumerate USB devices")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&discovery)?);
        return Ok(exit::ok());
    }
    print_human(&discovery);
    Ok(exit::ok())
}

fn print_human(discovery: &Discovery) {
    if discovery.is_empty() {
        println!("No TRYX displays found.");
        return;
    }
    if !discovery.printer_devices.is_empty() {
        let rows: Vec<Vec<String>> = discovery
            .printer_devices
            .iter()
            .map(|device| {
                let product = match device.product {
                    Some(product) => product.name().to_string(),
                    None if device.transitional => "Rockchip gadget (booting)".to_string(),
                    None => "unknown TRYX product".to_string(),
                };
                vec![
                    device.id.clone(),
                    product,
                    device.usb_id.clone(),
                    device.serial.clone().unwrap_or_else(|| "-".to_string()),
                    device.access.to_string(),
                    device.interface.to_string(),
                ]
            })
            .collect();
        print!(
            "{}",
            output::table(
                &[
                    "ID",
                    "PRODUCT",
                    "USB ID",
                    "SERIAL",
                    "ACCESS",
                    "PRINTER INTERFACE"
                ],
                &rows
            )
        );
    }
    if !discovery.legacy_devices.is_empty() {
        if !discovery.printer_devices.is_empty() {
            println!();
        }
        println!("Legacy cm01 firmware (serial + ADB protocol):");
        let rows: Vec<Vec<String>> = discovery
            .legacy_devices
            .iter()
            .map(|device| {
                vec![
                    device.id.clone(),
                    device.product_string.clone(),
                    device.usb_id.clone(),
                    device.serial.clone().unwrap_or_else(|| "-".to_string()),
                    device.tty.clone().unwrap_or_else(|| "-".to_string()),
                    device
                        .tty_access
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "-".to_string()),
                    if device.adb_interface { "yes" } else { "no" }.to_string(),
                ]
            })
            .collect();
        print!(
            "{}",
            output::table(
                &[
                    "ID",
                    "PRODUCT",
                    "USB ID",
                    "SERIAL",
                    "PORT",
                    "PORT ACCESS",
                    "ADB"
                ],
                &rows
            )
        );
    }
}
