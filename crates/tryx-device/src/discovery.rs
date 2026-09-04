//! USB discovery for TRYX displays.
//!
//! Printer-class displays (KANALI firmware) are enumerated with libusb and
//! identified by vendor `391a`. Displays still on the legacy `cm01` firmware
//! enumerate as an Android Open Accessory device (`18d1:2d04`) and are
//! recognised on Linux from sysfs by their `cm01*` product string.

use crate::product::{Product, ROCKCHIP_GADGET_PRODUCT_ID, VENDOR_ID};
use rusb::UsbContext;
use serde::Serialize;
use std::fmt;

/// Vendor ID (Google's Android Open Accessory range) used by the legacy firmware.
pub const LEGACY_VENDOR_ID: u16 = 0x18d1;
/// Product-string prefix reported by the legacy firmware, e.g. `cm01_se`.
pub const LEGACY_PRODUCT_PREFIX: &str = "cm01";

const PRINTER_INTERFACE_CLASS: u8 = 0x07;
const PRINTER_INTERFACE_SUBCLASS: u8 = 0x01;
const PRINTER_INTERFACE_PROTOCOL: u8 = 0x02;

#[cfg(target_os = "linux")]
const ADB_INTERFACE_CLASS: u8 = 0xff;
#[cfg(target_os = "linux")]
const ADB_INTERFACE_SUBCLASS: u8 = 0x42;
#[cfg(target_os = "linux")]
const ADB_INTERFACE_PROTOCOL: u8 = 0x01;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Access {
    Accessible,
    PermissionDenied,
    Busy,
    Error { message: String },
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Access::Accessible => f.write_str("ok"),
            Access::PermissionDenied => f.write_str("permission denied"),
            Access::Busy => f.write_str("busy"),
            Access::Error { message } => write!(f, "error: {message}"),
        }
    }
}

/// The vendor protocol interface: USB printer class with one bulk endpoint
/// in each direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrinterInterface {
    pub number: u8,
    pub alternate_setting: u8,
    pub bulk_in: u8,
    pub bulk_out: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum InterfaceStatus {
    Found { interface: PrinterInterface },
    Missing,
    Ambiguous { count: usize },
}

impl fmt::Display for InterfaceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterfaceStatus::Found { interface } => write!(
                f,
                "{} (in {:#04x}, out {:#04x})",
                interface.number, interface.bulk_in, interface.bulk_out
            ),
            InterfaceStatus::Missing => f.write_str("missing"),
            InterfaceStatus::Ambiguous { count } => write!(f, "ambiguous ({count})"),
        }
    }
}

/// A display running the KANALI printer-class firmware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrinterDevice {
    /// Stable identity from bus and port chain, e.g. `usb:003-12`.
    pub id: String,
    pub sysfs_path: Option<String>,
    pub usb_id: String,
    pub product_id: u16,
    /// `None` for the transitional Rockchip gadget.
    pub product: Option<Product>,
    pub transitional: bool,
    pub manufacturer: Option<String>,
    pub product_string: Option<String>,
    pub serial: Option<String>,
    pub access: Access,
    pub interface: InterfaceStatus,
}

/// A cooler still running the pre-KANALI `cm01` firmware, which speaks the
/// legacy JSON-over-serial protocol and transfers media over ADB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyDevice {
    pub id: String,
    pub sysfs_path: String,
    pub usb_id: String,
    pub manufacturer: Option<String>,
    pub product_string: String,
    pub serial: Option<String>,
    /// CDC ACM port carrying the command protocol, e.g. `/dev/ttyACM0`.
    pub tty: Option<String>,
    pub tty_access: Option<Access>,
    /// Whether the ADB interface (class `ff/42/01`) used for media transfer
    /// is exposed.
    pub adb_interface: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Discovery {
    pub printer_devices: Vec<PrinterDevice>,
    pub legacy_devices: Vec<LegacyDevice>,
    /// Why printer-class enumeration was impossible, e.g. no USB subsystem
    /// in a container. Legacy devices are still found through sysfs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb_error: Option<String>,
}

impl Discovery {
    pub fn is_empty(&self) -> bool {
        self.printer_devices.is_empty() && self.legacy_devices.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("USB enumeration failed: {0}")]
    Usb(#[from] rusb::Error),
}

pub fn discover() -> Result<Discovery, DiscoveryError> {
    let (printer_devices, usb_error) = match printer_devices() {
        Ok(devices) => (devices, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    Ok(Discovery {
        printer_devices,
        legacy_devices: legacy_devices(),
        usb_error,
    })
}

/// Stable device identity from the bus number and port chain, falling back to
/// the (unstable) device address when the port chain is unknown. Matches the
/// upstream `devicePath` format.
pub fn stable_id(bus: u8, ports: &[u8], address: u8) -> String {
    if ports.is_empty() {
        format!("usb:{bus:03}@{address:03}")
    } else {
        format!("usb:{bus:03}-{}", port_chain(ports))
    }
}

/// Linux sysfs directory for a device, e.g. `/sys/bus/usb/devices/3-12.1`.
pub fn sysfs_path(bus: u8, ports: &[u8]) -> Option<String> {
    if !cfg!(target_os = "linux") || ports.is_empty() {
        return None;
    }
    Some(format!("/sys/bus/usb/devices/{bus}-{}", port_chain(ports)))
}

/// Parses a sysfs device name such as `3-12.1` into its bus and port chain.
pub fn parse_sysfs_name(name: &str) -> Option<(u8, Vec<u8>)> {
    let (bus, ports) = name.split_once('-')?;
    let bus = bus.parse().ok()?;
    let ports = ports
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<u8>, _>>()
        .ok()?;
    (!ports.is_empty()).then_some((bus, ports))
}

fn port_chain(ports: &[u8]) -> String {
    ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

#[derive(Default)]
struct Strings {
    manufacturer: Option<String>,
    product: Option<String>,
    serial: Option<String>,
}

fn printer_devices() -> Result<Vec<PrinterDevice>, rusb::Error> {
    // An explicit context: the global one aborts the process when libusb
    // cannot initialise, which happens wherever /dev/bus/usb is absent.
    let context = rusb::Context::new()?;
    let mut found = Vec::new();
    for device in context.devices()?.iter() {
        let Ok(descriptor) = device.device_descriptor() else {
            continue;
        };
        if descriptor.vendor_id() != VENDOR_ID {
            continue;
        }
        found.push(describe_printer_device(&device, &descriptor));
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(found)
}

fn describe_printer_device(
    device: &rusb::Device<rusb::Context>,
    descriptor: &rusb::DeviceDescriptor,
) -> PrinterDevice {
    let ports = device.port_numbers().unwrap_or_default();
    let bus = device.bus_number();
    let product_id = descriptor.product_id();
    let interface = match device.active_config_descriptor() {
        Ok(config) => find_printer_interface(&config),
        Err(_) => InterfaceStatus::Missing,
    };
    let (access, strings) = match device.open() {
        Ok(handle) => (Access::Accessible, read_strings(&handle, descriptor)),
        Err(rusb::Error::Access) => (Access::PermissionDenied, Strings::default()),
        Err(rusb::Error::Busy) => (Access::Busy, Strings::default()),
        Err(error) => (
            Access::Error {
                message: error.to_string(),
            },
            Strings::default(),
        ),
    };
    let sysfs_path = sysfs_path(bus, &ports);
    let strings = with_sysfs_fallback(strings, sysfs_path.as_deref());
    PrinterDevice {
        id: stable_id(bus, &ports, device.address()),
        sysfs_path,
        usb_id: format!("{VENDOR_ID:04x}:{product_id:04x}"),
        product_id,
        product: Product::from_product_id(product_id),
        transitional: product_id == ROCKCHIP_GADGET_PRODUCT_ID,
        manufacturer: strings.manufacturer,
        product_string: strings.product,
        serial: strings.serial,
        access,
        interface,
    }
}

/// String descriptors are readable from sysfs without opening the device, so
/// a display we lack permission for is still named in listings.
#[cfg(target_os = "linux")]
fn with_sysfs_fallback(mut strings: Strings, sysfs_path: Option<&str>) -> Strings {
    if let Some(path) = sysfs_path {
        let path = std::path::Path::new(path);
        strings.manufacturer = strings
            .manufacturer
            .or_else(|| sysfs::read_attr(path, "manufacturer"));
        strings.product = strings
            .product
            .or_else(|| sysfs::read_attr(path, "product"));
        strings.serial = strings.serial.or_else(|| sysfs::read_attr(path, "serial"));
    }
    strings
}

#[cfg(not(target_os = "linux"))]
fn with_sysfs_fallback(strings: Strings, _sysfs_path: Option<&str>) -> Strings {
    strings
}

fn read_strings(
    handle: &rusb::DeviceHandle<rusb::Context>,
    descriptor: &rusb::DeviceDescriptor,
) -> Strings {
    Strings {
        manufacturer: handle.read_manufacturer_string_ascii(descriptor).ok(),
        product: handle.read_product_string_ascii(descriptor).ok(),
        serial: handle.read_serial_number_string_ascii(descriptor).ok(),
    }
}

/// Finds the single printer-class interface exposing exactly one bulk IN and
/// one bulk OUT endpoint, as the upstream transport requires.
pub fn find_printer_interface(config: &rusb::ConfigDescriptor) -> InterfaceStatus {
    let mut matches = Vec::new();
    for interface in config.interfaces() {
        for setting in interface.descriptors() {
            if setting.class_code() != PRINTER_INTERFACE_CLASS
                || setting.sub_class_code() != PRINTER_INTERFACE_SUBCLASS
                || setting.protocol_code() != PRINTER_INTERFACE_PROTOCOL
            {
                continue;
            }
            let mut bulk_in = Vec::new();
            let mut bulk_out = Vec::new();
            for endpoint in setting.endpoint_descriptors() {
                if endpoint.transfer_type() != rusb::TransferType::Bulk {
                    continue;
                }
                match endpoint.direction() {
                    rusb::Direction::In => bulk_in.push(endpoint.address()),
                    rusb::Direction::Out => bulk_out.push(endpoint.address()),
                }
            }
            if let ([bulk_in], [bulk_out]) = (bulk_in.as_slice(), bulk_out.as_slice()) {
                matches.push(PrinterInterface {
                    number: setting.interface_number(),
                    alternate_setting: setting.setting_number(),
                    bulk_in: *bulk_in,
                    bulk_out: *bulk_out,
                });
            }
        }
    }
    match matches.len() {
        0 => InterfaceStatus::Missing,
        1 => InterfaceStatus::Found {
            interface: matches.remove(0),
        },
        count => InterfaceStatus::Ambiguous { count },
    }
}

#[cfg(target_os = "linux")]
fn legacy_devices() -> Vec<LegacyDevice> {
    use std::path::Path;

    let Ok(entries) = std::fs::read_dir(sysfs::ROOT) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((bus, ports)) = parse_sysfs_name(&name) else {
            continue;
        };
        if sysfs::read_hex_u16(&path, "idVendor") != Some(LEGACY_VENDOR_ID) {
            continue;
        }
        let Some(product_string) = sysfs::read_attr(&path, "product") else {
            continue;
        };
        if !product_string.starts_with(LEGACY_PRODUCT_PREFIX) {
            continue;
        }
        let product_id = sysfs::read_hex_u16(&path, "idProduct").unwrap_or(0);
        let address = sysfs::read_attr(&path, "devnum")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut tty = None;
        let mut adb_interface = false;
        for interface in sysfs::interfaces(&path) {
            match sysfs::interface_class(&interface) {
                Some((ADB_INTERFACE_CLASS, ADB_INTERFACE_SUBCLASS, ADB_INTERFACE_PROTOCOL)) => {
                    adb_interface = true;
                }
                _ => {
                    if tty.is_none() {
                        tty = sysfs::tty_node(&interface);
                    }
                }
            }
        }
        let tty_access = tty.as_deref().map(|node| path_access(Path::new(node)));
        found.push(LegacyDevice {
            id: stable_id(bus, &ports, address),
            sysfs_path: path.to_string_lossy().into_owned(),
            usb_id: format!("{LEGACY_VENDOR_ID:04x}:{product_id:04x}"),
            manufacturer: sysfs::read_attr(&path, "manufacturer"),
            product_string,
            serial: sysfs::read_attr(&path, "serial"),
            tty,
            tty_access,
            adb_interface,
        });
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

#[cfg(not(target_os = "linux"))]
fn legacy_devices() -> Vec<LegacyDevice> {
    Vec::new()
}

/// Read/write permission on a device node, checked without opening it so a
/// listing never toggles a serial port's control lines.
#[cfg(target_os = "linux")]
fn path_access(path: &std::path::Path) -> Access {
    use nix::unistd::{AccessFlags, access};

    match access(path, AccessFlags::R_OK | AccessFlags::W_OK) {
        Ok(()) => Access::Accessible,
        Err(nix::errno::Errno::EACCES) => Access::PermissionDenied,
        Err(error) => Access::Error {
            message: error.to_string(),
        },
    }
}

#[cfg(target_os = "linux")]
mod sysfs {
    use std::path::{Path, PathBuf};

    pub const ROOT: &str = "/sys/bus/usb/devices";

    pub fn read_attr(device: &Path, name: &str) -> Option<String> {
        let value = std::fs::read_to_string(device.join(name)).ok()?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    }

    pub fn read_hex_u16(device: &Path, name: &str) -> Option<u16> {
        u16::from_str_radix(&read_attr(device, name)?, 16).ok()
    }

    fn read_hex_u8(device: &Path, name: &str) -> Option<u8> {
        u8::from_str_radix(&read_attr(device, name)?, 16).ok()
    }

    /// Interface directories of `device`, named `<device>:<config>.<number>`.
    pub fn interfaces(device: &Path) -> Vec<PathBuf> {
        let prefix = format!(
            "{}:",
            device.file_name().unwrap_or_default().to_string_lossy()
        );
        let Ok(entries) = std::fs::read_dir(device) else {
            return Vec::new();
        };
        let mut interfaces: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .map(|entry| entry.path())
            .collect();
        interfaces.sort();
        interfaces
    }

    pub fn interface_class(interface: &Path) -> Option<(u8, u8, u8)> {
        Some((
            read_hex_u8(interface, "bInterfaceClass")?,
            read_hex_u8(interface, "bInterfaceSubClass")?,
            read_hex_u8(interface, "bInterfaceProtocol")?,
        ))
    }

    pub fn tty_node(interface: &Path) -> Option<String> {
        let entry = std::fs::read_dir(interface.join("tty"))
            .ok()?
            .flatten()
            .next()?;
        Some(format!("/dev/{}", entry.file_name().to_string_lossy()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_uses_bus_and_port_chain() {
        assert_eq!(stable_id(3, &[12], 4), "usb:003-12");
        assert_eq!(stable_id(1, &[2, 3, 1], 9), "usb:001-2.3.1");
    }

    #[test]
    fn stable_id_falls_back_to_address_without_ports() {
        assert_eq!(stable_id(1, &[], 9), "usb:001@009");
    }

    #[test]
    fn sysfs_name_parses_bus_and_ports() {
        assert_eq!(parse_sysfs_name("3-12"), Some((3, vec![12])));
        assert_eq!(parse_sysfs_name("1-2.3.1"), Some((1, vec![2, 3, 1])));
        assert_eq!(parse_sysfs_name("usb3"), None);
        assert_eq!(parse_sysfs_name("3-12:1.0"), None);
        assert_eq!(parse_sysfs_name("3-"), None);
    }

    #[test]
    fn sysfs_path_is_linux_only() {
        let path = sysfs_path(3, &[12, 1]);
        if cfg!(target_os = "linux") {
            assert_eq!(path.as_deref(), Some("/sys/bus/usb/devices/3-12.1"));
        } else {
            assert_eq!(path, None);
        }
        assert_eq!(sysfs_path(3, &[]), None);
    }
}
