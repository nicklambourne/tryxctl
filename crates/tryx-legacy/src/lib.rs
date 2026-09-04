//! Protocol of the original `cm01` firmware on TRYX Panorama coolers.
//!
//! Commands travel as byte-stuffed text frames carrying JSON over a CDC ACM
//! serial port ([`frame`], [`serial`]); media files are transferred with
//! `adb` ([`adb`]). Ported from `src/core/{protocol,device,adb}.cpp` in
//! DXVSI/Tryx-Linux-GUI.

pub mod adb;
pub mod commands;
pub mod frame;
pub mod serial;

use serde::{Deserialize, Serialize};
use std::thread;
use std::time::Duration;

pub use commands::{DisplaySettings, PcInfo, ScreenConfig, local_utc_offset_ms};
pub use frame::Response;
pub use serial::SerialLink;

#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    #[error("serial port {path}: {source}")]
    Serial {
        path: String,
        #[source]
        source: serialport::Error,
    },
    #[error("serial I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("request too long for a legacy frame ({0} bytes)")]
    RequestTooLong(usize),
    #[error("no response to `{command}` within {timeout_ms} ms")]
    NoResponse { command: String, timeout_ms: u64 },
    #[error("malformed response to `{command}`")]
    MalformedResponse { command: String },
    #[error("`{command}` returned no JSON body")]
    MissingJson { command: String },
    #[error("adb is not installed")]
    AdbMissing,
    #[error("adb {args}: {message}")]
    Adb { args: String, message: String },
    #[error(
        "media name {0:?} is not safe: use ASCII letters, digits, '.', '_' or '-' and no leading dot"
    )]
    UnsafeMediaName(String),
}

/// Identity reported by the `conn` handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub product_id: String,
    pub os: String,
    pub serial: String,
    pub app_version: String,
    pub firmware: String,
    pub hardware: String,
    pub attributes: Vec<String>,
}

/// Fan readings the firmware returns in reply to a sysinfo message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanStatus {
    /// `status.fanLCD`: the fan on the display block.
    pub lcd_fan_rpm: Option<u32>,
    /// `status.turboPump`: reported by some models only.
    pub pump_rpm: Option<u32>,
}

impl FanStatus {
    /// Reads `status.fanLCD` and `status.turboPump`, each a number or a
    /// string of digits depending on firmware.
    pub fn from_json(json: &serde_json::Value) -> FanStatus {
        let field = |name: &str| -> Option<u32> {
            let value = json.get("status")?.get(name)?;
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .or_else(|| {
                    value
                        .as_f64()
                        .filter(|f| f.is_finite() && *f >= 0.0)
                        .map(|f| f.round() as u32)
                })
                .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        };
        FanStatus {
            lcd_fan_rpm: field("fanLCD"),
            pump_rpm: field("turboPump"),
        }
    }
}

/// Delay the device needs between the two `waterBlockScreenId` sends.
const SCREEN_CONFIG_REPEAT_DELAY: Duration = Duration::from_millis(200);

/// A connected legacy display.
pub struct Client {
    link: SerialLink,
}

impl Client {
    pub fn open(tty: &str) -> Result<Self, LegacyError> {
        Ok(Client {
            link: SerialLink::open(tty)?,
        })
    }

    pub fn from_link(link: SerialLink) -> Self {
        Client { link }
    }

    /// Dump every frame on the wire to stderr.
    pub fn set_trace(&mut self, trace: bool) {
        self.link.trace = trace;
    }

    /// The underlying link, for sending commands the client does not model.
    pub fn link_mut(&mut self) -> &mut SerialLink {
        &mut self.link
    }

    /// `POST conn`: identifies the device.
    pub fn handshake(&mut self) -> Result<DeviceInfo, LegacyError> {
        let response = self.link.request("conn", "")?;
        let json = response.json.ok_or_else(|| LegacyError::MissingJson {
            command: "conn".to_string(),
        })?;
        Ok(DeviceInfo::from_json(&json))
    }

    /// Backlight brightness, clamped to 0..=100.
    pub fn set_brightness(&mut self, value: u8) -> Result<Response, LegacyError> {
        let body = commands::brightness(value.min(100));
        self.link.request("brightness", &body.to_string())
    }

    pub fn set_waterfall_mode(&mut self, enable: bool) -> Result<Response, LegacyError> {
        self.link.request(
            "waterfallMode",
            &commands::waterfall_mode(enable).to_string(),
        )
    }

    pub fn set_rotation(&mut self, degrees: u16) -> Result<Response, LegacyError> {
        self.link
            .request("rotate", &commands::rotate(degrees).to_string())
    }

    pub fn reboot(&mut self) -> Result<Response, LegacyError> {
        self.link.request("reboot", "")
    }

    pub fn set_temperature_unit(&mut self, unit: &str) -> Result<Response, LegacyError> {
        self.link
            .request("temperature", &commands::temperature_unit(unit).to_string())
    }

    /// `POST sysinfoDisplay`: the overlay's metric labels. Fire-and-forget
    /// like upstream.
    pub fn set_sysinfo_display(&mut self, labels: &[String]) -> Result<(), LegacyError> {
        self.link.send(
            "sysinfoDisplay",
            &commands::sysinfo_display(labels).to_string(),
        )
    }

    pub fn send_spec(&mut self, cpu: &str, gpu: &str) -> Result<Response, LegacyError> {
        self.link
            .request("spec", &commands::spec(cpu, gpu).to_string())
    }

    /// `POST mediaDelete`: asks the firmware to forget the named files.
    pub fn delete_media(&mut self, files: &[String]) -> Result<Response, LegacyError> {
        self.link
            .request("mediaDelete", &commands::media_delete(files).to_string())
    }

    /// `POST waterBlockScreenId`, sent twice because the device applies it
    /// reliably only on the second send, followed by the waterfall mode.
    pub fn set_screen_config(&mut self, config: &ScreenConfig) -> Result<Response, LegacyError> {
        let body = commands::screen_config(config).to_string();
        self.link.request("waterBlockScreenId", &body)?;
        thread::sleep(SCREEN_CONFIG_REPEAT_DELAY);
        let response = self.link.request("waterBlockScreenId", &body)?;
        thread::sleep(SCREEN_CONFIG_REPEAT_DELAY);
        self.set_waterfall_mode(config.waterfall_mode)?;
        Ok(response)
    }

    /// `POST config`: the combined configuration in the vendor app's format.
    pub fn send_full_config(
        &mut self,
        config: &ScreenConfig,
        cpu: &str,
        gpu: &str,
        brightness: u8,
        temperature_unit: &str,
    ) -> Result<Response, LegacyError> {
        let body = commands::full_config(config, cpu, gpu, brightness.min(100), temperature_unit);
        self.link.request("config", &body.to_string())
    }

    /// `POST all`: live system metrics, which also keeps the panel awake.
    /// The reply carries the fan readings; a missing reply is not an error.
    pub fn send_sysinfo(&mut self, info: &PcInfo) -> Result<FanStatus, LegacyError> {
        match self
            .link
            .request("all", &commands::pc_info(info).to_string())
        {
            Ok(response) => Ok(response
                .json
                .as_ref()
                .map(FanStatus::from_json)
                .unwrap_or_default()),
            Err(LegacyError::NoResponse { .. }) => Ok(FanStatus::default()),
            Err(error) => Err(error),
        }
    }

    /// `POST fanLCDSet`: fixed speed for the display-block fan, 0 to 100.
    pub fn set_fan_lcd(&mut self, percent: u8) -> Result<Response, LegacyError> {
        self.link
            .request("fanLCDSet", &commands::fan_lcd(percent).to_string())
    }
}

impl DeviceInfo {
    fn from_json(json: &serde_json::Value) -> DeviceInfo {
        let text = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string()
        };
        let version = json
            .get("version")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        DeviceInfo {
            product_id: text(json, "productId"),
            os: text(json, "OS"),
            serial: text(json, "sn"),
            app_version: text(&version, "app"),
            firmware: text(&version, "firmware"),
            hardware: text(&version, "hardware"),
            attributes: json
                .get("attribute")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_info_reads_the_conn_reply() {
        let json = json!({
            "productId": "cm01_se",
            "OS": "Android",
            "sn": "XYZ000000000000001",
            "version": {"app": "1.2.3", "firmware": "2.0", "hardware": "A"},
            "attribute": ["waterBlockScreen", 7, "argb"]
        });
        assert_eq!(
            DeviceInfo::from_json(&json),
            DeviceInfo {
                product_id: "cm01_se".into(),
                os: "Android".into(),
                serial: "XYZ000000000000001".into(),
                app_version: "1.2.3".into(),
                firmware: "2.0".into(),
                hardware: "A".into(),
                attributes: vec!["waterBlockScreen".into(), "argb".into()],
            }
        );
    }

    #[test]
    fn fan_status_reads_numbers_or_digit_strings() {
        let status =
            FanStatus::from_json(&json!({"status": {"fanLCD": "1280", "turboPump": 2400}}));
        assert_eq!(
            status,
            FanStatus {
                lcd_fan_rpm: Some(1280),
                pump_rpm: Some(2400)
            }
        );
        assert_eq!(
            FanStatus::from_json(&json!({"status": {"fanLCD": "0"}})),
            FanStatus {
                lcd_fan_rpm: Some(0),
                pump_rpm: None
            }
        );
        assert_eq!(FanStatus::from_json(&json!({})), FanStatus::default());
    }

    #[test]
    fn device_info_defaults_missing_fields_to_unknown() {
        let info = DeviceInfo::from_json(&json!({"productId": "cm01_se"}));
        assert_eq!(info.product_id, "cm01_se");
        assert_eq!(info.os, "unknown");
        assert_eq!(info.firmware, "unknown");
        assert!(info.attributes.is_empty());
    }
}
