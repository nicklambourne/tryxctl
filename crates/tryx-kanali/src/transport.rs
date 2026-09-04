//! The bulk USB pipe, behind a trait so tests can substitute a socket.

use rusb::UsbContext;
use std::time::Duration;
use tryx_device::Product;
use tryx_device::discovery::{self, InterfaceStatus, PrinterInterface};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("timed out")]
    Timeout,
    #[error("the device is gone")]
    Disconnected,
    #[error("usb: {0}")]
    Usb(String),
    #[error("io: {0}")]
    Io(String),
    #[error("{0}")]
    NotFound(String),
}

pub trait Pipe: Send {
    /// Writes `data`; returns the byte count actually accepted.
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, TransportError>;
    /// Reads into `buffer`; `Ok(0)` never means end of stream here.
    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, TransportError>;
}

pub struct UsbPipe {
    handle: rusb::DeviceHandle<rusb::Context>,
    interface: PrinterInterface,
    reattach: bool,
}

impl UsbPipe {
    /// Opens the printer-class interface of the device whose discovery id
    /// matches `id`, detaching `usblp` if the kernel bound it.
    pub fn open(id: &str) -> Result<(Product, UsbPipe), TransportError> {
        let context = rusb::Context::new().map_err(usb)?;
        for device in context.devices().map_err(usb)?.iter() {
            let Ok(descriptor) = device.device_descriptor() else {
                continue;
            };
            if descriptor.vendor_id() != tryx_device::product::VENDOR_ID {
                continue;
            }
            let ports = device.port_numbers().unwrap_or_default();
            if discovery::stable_id(device.bus_number(), &ports, device.address()) != id {
                continue;
            }
            let product = Product::from_product_id(descriptor.product_id()).ok_or_else(|| {
                TransportError::NotFound(format!("{id} is not a supported product"))
            })?;
            let config = device.active_config_descriptor().map_err(usb)?;
            let interface = match discovery::find_printer_interface(&config) {
                InterfaceStatus::Found { interface } => interface,
                other => {
                    return Err(TransportError::NotFound(format!(
                        "{id}: printer interface {other}"
                    )));
                }
            };
            let handle = device.open().map_err(usb)?;
            let reattach = handle
                .kernel_driver_active(interface.number)
                .unwrap_or(false);
            if reattach {
                handle.detach_kernel_driver(interface.number).map_err(usb)?;
            }
            handle.claim_interface(interface.number).map_err(usb)?;
            let _ = handle.set_alternate_setting(interface.number, interface.alternate_setting);
            return Ok((
                product,
                UsbPipe {
                    handle,
                    interface,
                    reattach,
                },
            ));
        }
        Err(TransportError::NotFound(format!(
            "no printer-class display with id {id}"
        )))
    }
}

impl Drop for UsbPipe {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface.number);
        if self.reattach {
            let _ = self.handle.attach_kernel_driver(self.interface.number);
        }
    }
}

fn usb(error: rusb::Error) -> TransportError {
    match error {
        rusb::Error::Timeout => TransportError::Timeout,
        rusb::Error::NoDevice | rusb::Error::Pipe => TransportError::Disconnected,
        other => TransportError::Usb(other.to_string()),
    }
}

impl Pipe for UsbPipe {
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, TransportError> {
        self.handle
            .write_bulk(self.interface.bulk_out, data, timeout)
            .map_err(usb)
    }

    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, TransportError> {
        self.handle
            .read_bulk(self.interface.bulk_in, buffer, timeout)
            .map_err(usb)
    }
}

impl Pipe for std::os::unix::net::UnixStream {
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, TransportError> {
        use std::io::Write;
        self.set_write_timeout(Some(timeout))
            .map_err(|e| TransportError::Io(e.to_string()))?;
        match Write::write(self, data) {
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Err(TransportError::Timeout)
            }
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                Err(TransportError::Disconnected)
            }
            Err(e) => Err(TransportError::Io(e.to_string())),
        }
    }

    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, TransportError> {
        use std::io::Read;
        self.set_read_timeout(Some(timeout))
            .map_err(|e| TransportError::Io(e.to_string()))?;
        match Read::read(self, buffer) {
            Ok(0) => Err(TransportError::Disconnected),
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Err(TransportError::Timeout)
            }
            Err(e) => Err(TransportError::Io(e.to_string())),
        }
    }
}
