//! The KANALI protocol spoken by TRYX displays on current firmware: a USB
//! printer-class interface carrying framed protobuf requests.
//!
//! Ported from DXVSI/Tryx-Linux-GUI `src/printerprotocol.cpp`. This
//! implementation is synchronous: one request is written, then the reply is
//! read, which is enough for a command-line tool. It has been exercised
//! against a scripted fake device only; see the crate tests.

pub mod overlay;
pub mod session;
pub mod transport;

use prost::Message;
use serde::{Deserialize, Serialize};
use session::{Expect, Profile, Session};
use std::path::Path;
use std::time::Duration;
use transport::Pipe;
use tryx_device::Product;
use tryx_proto::wire::v1 as wire;
use wire::{request, response};

pub use overlay::{OverlayArea, OverlayConfig};

/// Bytes per transfer chunk (`kFileTransmitChunkSize`).
pub const CHUNK_SIZE: usize = 0x40000;
/// Largest upload the firmware accepts.
pub const MAX_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;
/// Turris transfers use this fixed track id instead of a random one.
pub const TURRIS_TRANSFER_TRACK_ID: u64 = 981_521;
/// The panel needs a keepalive at least this often once a session is open.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// Overall budget for the readiness loop after connecting.
pub const READINESS_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum KanaliError {
    #[error(transparent)]
    Transport(#[from] transport::TransportError),
    #[error("the device did not answer `{request}` within {timeout_ms} ms")]
    NoResponse {
        request: &'static str,
        timeout_ms: u64,
    },
    #[error("the device rejected `{request}`: {why}")]
    Rejected { request: &'static str, why: String },
    #[error("unexpected reply to `{request}`: {detail}")]
    InvalidResponse {
        request: &'static str,
        detail: String,
    },
    #[error("{0} is not supported by this product")]
    Unsupported(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("transfer failed: {0}")]
    Transfer(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("protobuf: {0}")]
    Encode(#[from] prost::EncodeError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub os_name: String,
    pub os_version: String,
    pub firmware_version: String,
    pub product_name: String,
    pub app_version: String,
    pub serial_number: String,
    pub serial_number_locked: bool,
    pub chip_id: String,
}

impl From<wire::DeviceInformation> for DeviceInfo {
    fn from(info: wire::DeviceInformation) -> Self {
        DeviceInfo {
            os_name: info.os_name,
            os_version: info.os_version,
            firmware_version: info.firmware_version,
            product_name: info.product_name,
            app_version: info.app_version,
            serial_number: info.serial_number,
            serial_number_locked: info.serial_number_locked,
            chip_id: info.chip_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEntry {
    pub name: String,
    pub extension: String,
    pub size: u32,
    pub read_only: bool,
    pub preset: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub user: Vec<MediaEntry>,
    pub presets: Vec<MediaEntry>,
}

/// What the device reports about its screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayState {
    pub backlight_enabled: bool,
    pub brightness: u32,
    pub standby_enabled: bool,
    pub standby_media: String,
    pub mirror: bool,
    pub waterfall: bool,
    /// `Full Screen`, `Screen Splitting`, or `Kaleidoscope`.
    pub screen_mode: String,
    /// `Single`, `Loop`, or `Shuffle`.
    pub play_mode: String,
    pub media: Vec<String>,
}

/// A change to apply; unset fields are left as the device has them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// Media names as listed in the catalog: one for full screen, two for
    /// split screen.
    pub media: Option<Vec<String>>,
    pub split_screen: bool,
    /// `Single`, `Loop`, or `Shuffle`; full screen only.
    pub play_mode: Option<String>,
    pub brightness: Option<u32>,
    pub backlight: Option<bool>,
    /// Portrait orientation (`ui_rotation` 90).
    pub waterfall: Option<bool>,
    /// Media rotation in degrees (`media_rotation`).
    pub rotation: Option<u32>,
}

pub struct Device {
    product: Product,
    session: Session,
}

impl Device {
    /// Opens the printer-class interface of the device with this discovery
    /// id, e.g. `usb:003-12`.
    pub fn open(id: &str) -> Result<Device, KanaliError> {
        if let Some(supplied) = transport::open_supplied(id) {
            let (product, stream) = supplied?;
            return Ok(Device::from_pipe(product, Box::new(stream)));
        }
        let (product, pipe) = transport::UsbPipe::open(id)?;
        Ok(Device::from_pipe(product, Box::new(pipe)))
    }

    pub fn from_pipe(product: Product, pipe: Box<dyn Pipe>) -> Device {
        Device {
            product,
            session: Session::new(pipe),
        }
    }

    pub fn product(&self) -> Product {
        self.product
    }

    pub fn set_trace(&mut self, trace: bool) {
        self.session.trace = trace;
    }

    fn require(&self, supported: bool, what: &'static str) -> Result<(), KanaliError> {
        if supported {
            Ok(())
        } else {
            Err(KanaliError::Unsupported(what))
        }
    }

    /// The readiness loop: asks for device information for up to twenty
    /// seconds, then the one-time system and authentication queries.
    pub fn start_session(&mut self) -> Result<DeviceInfo, KanaliError> {
        self.require(
            self.product.capabilities().media_catalog,
            "a display session",
        )?;
        let info = self.session.bootstrap()?;
        Ok(DeviceInfo::from(info))
    }

    pub fn catalog(&mut self) -> Result<Catalog, KanaliError> {
        self.require(
            self.product.capabilities().media_catalog,
            "the media catalog",
        )?;
        let mut request = wire::Request {
            body: Some(request::Body::MediaCatalogQuery(wire::QueryToken {
                dummy: "NA".into(),
            })),
            ..Default::default()
        };
        let response =
            self.session
                .execute(&mut request, Expect::MediaCatalog, Profile::Normal, None)?;
        let Some(response::Body::MediaCatalog(catalog)) = response.body else {
            unreachable!("execute checked the body");
        };
        let entry = |item: wire::MediaEntry, preset: bool| MediaEntry {
            name: item.file_path,
            extension: item.file_ext,
            size: item.file_size,
            read_only: item.read_only,
            preset,
        };
        Ok(Catalog {
            user: catalog
                .media_file_list
                .into_iter()
                .map(|item| entry(item, false))
                .collect(),
            presets: catalog
                .preset_file_list
                .into_iter()
                .map(|item| entry(item, true))
                .collect(),
        })
    }

    fn user_configuration(&mut self) -> Result<wire::UserConfiguration, KanaliError> {
        let mut request = wire::Request {
            body: Some(request::Body::UserConfigurationQuery(wire::QueryToken {
                dummy: "NA".into(),
            })),
            ..Default::default()
        };
        let response = self.session.execute(
            &mut request,
            Expect::UserConfiguration,
            Profile::Normal,
            None,
        )?;
        match response.body {
            Some(response::Body::UserConfiguration(config)) => Ok(config),
            _ => unreachable!("execute checked the body"),
        }
    }

    pub fn display_state(&mut self) -> Result<DisplayState, KanaliError> {
        self.require(
            self.product.capabilities().display_configuration,
            "display configuration",
        )?;
        let config = self.user_configuration()?;
        state_from(&config).ok_or_else(|| KanaliError::InvalidResponse {
            request: "user_configuration_query",
            detail: "no display or work configuration".into(),
        })
    }

    /// Read-modify-write of the user configuration, then activation with
    /// the overlay layout, then a readback that must match.
    pub fn apply(
        &mut self,
        change: &Change,
        overlay: Option<&OverlayConfig>,
    ) -> Result<DisplayState, KanaliError> {
        self.require(
            self.product.capabilities().display_configuration,
            "display configuration",
        )?;
        if change.media.is_none()
            && change.brightness.is_none()
            && change.backlight.is_none()
            && change.waterfall.is_none()
            && change.rotation.is_none()
        {
            return Err(KanaliError::Invalid("the change is empty".into()));
        }
        if let Some(rotation) = change.rotation
            && !matches!(rotation, 0 | 90 | 180 | 270)
        {
            return Err(KanaliError::Invalid(
                "rotation must be 0, 90, 180, or 270".into(),
            ));
        }
        if let Some(brightness) = change.brightness
            && brightness > 100
        {
            return Err(KanaliError::Invalid("brightness must be 0 to 100".into()));
        }
        let mut config = self.user_configuration()?;
        if let Some(media) = &change.media {
            let expected = if change.split_screen { 2 } else { 1 };
            if media.len() != expected {
                return Err(KanaliError::Invalid(format!(
                    "this screen mode needs {expected} media file(s)"
                )));
            }
            if let Some(name) = media.iter().find(|name| !is_safe_device_name(name)) {
                return Err(KanaliError::Invalid(format!(
                    "media name {name:?} is not accepted by the device"
                )));
            }
            let work = config.work_config.get_or_insert_with(Default::default);
            if change.split_screen {
                work.media_mode = wire::work_configuration::MediaMode::MediaDual as i32;
                work.loop_mode = wire::work_configuration::LoopMode::LoopSingle as i32;
                work.dual_mode_left_media_file = media[0].clone();
                work.dual_mode_right_media_file = media[1].clone();
            } else {
                work.media_mode = wire::work_configuration::MediaMode::MediaSingle as i32;
                work.loop_mode = match change.play_mode.as_deref() {
                    Some("Loop") => wire::work_configuration::LoopMode::LoopAll,
                    Some("Shuffle") => wire::work_configuration::LoopMode::LoopRandom,
                    _ => wire::work_configuration::LoopMode::LoopSingle,
                } as i32;
                work.single_mode_media_file = media[0].clone();
            }
        }
        if change.brightness.is_some()
            || change.backlight.is_some()
            || change.waterfall.is_some()
            || change.rotation.is_some()
        {
            let display = config.display_config.get_or_insert_with(Default::default);
            if let Some(brightness) = change.brightness {
                display.backlight_brightness = brightness;
            }
            if let Some(backlight) = change.backlight {
                display.backlight_enable = backlight;
            }
            if let Some(waterfall) = change.waterfall {
                display.ui_rotation = if waterfall { 90 } else { 0 };
            }
            if let Some(rotation) = change.rotation {
                display.media_rotation = rotation;
            }
        }

        let mut write = wire::Request {
            body: Some(request::Body::UserConfiguration(config)),
            ..Default::default()
        };
        self.session
            .execute(&mut write, Expect::Acknowledgement, Profile::Normal, None)?;
        self.activate(overlay)?;

        let state = self.display_state()?;
        let mut mismatches = Vec::new();
        if let Some(media) = &change.media {
            if &state.media != media {
                mismatches.push("media");
            }
            let expected_mode = if change.split_screen {
                "Screen Splitting"
            } else {
                "Full Screen"
            };
            if state.screen_mode != expected_mode {
                mismatches.push("screen mode");
            }
        }
        if let Some(brightness) = change.brightness
            && state.brightness != brightness
        {
            mismatches.push("brightness");
        }
        if let Some(waterfall) = change.waterfall
            && state.waterfall != waterfall
        {
            mismatches.push("waterfall");
        }
        if !mismatches.is_empty() {
            return Err(KanaliError::InvalidResponse {
                request: "user_configuration",
                detail: format!("device readback does not match: {}", mismatches.join(", ")),
            });
        }
        Ok(state)
    }

    /// The activation trigger: an overlay layout (empty without metrics)
    /// written after the configuration was acknowledged.
    fn activate(&mut self, overlay: Option<&OverlayConfig>) -> Result<(), KanaliError> {
        let layout = overlay.map(overlay::run_config).unwrap_or_default();
        let mut request = wire::Request {
            body: Some(request::Body::OverlayLayout(layout)),
            ..Default::default()
        };
        self.session.write_tracked_only(&mut request)?;
        Ok(())
    }

    pub fn set_brightness(&mut self, brightness: u32) -> Result<DisplayState, KanaliError> {
        self.apply(
            &Change {
                brightness: Some(brightness),
                ..Change::default()
            },
            None,
        )
    }

    /// Sends the overlay layout on its own, without touching the media.
    pub fn configure_overlay(&mut self, overlay: &OverlayConfig) -> Result<(), KanaliError> {
        self.require(
            self.product.capabilities().overlay_metrics,
            "overlay metrics",
        )?;
        self.activate(Some(overlay))
    }

    /// Live values for the labels in `overlay`; fire-and-forget.
    pub fn send_metrics(
        &mut self,
        overlay: &OverlayConfig,
        values: &[overlay::MetricValue],
    ) -> Result<(), KanaliError> {
        self.require(
            self.product.capabilities().overlay_metrics,
            "overlay metrics",
        )?;
        let Some(batch) = overlay::metric_batch(overlay, values) else {
            return Ok(());
        };
        let request = wire::Request {
            body: Some(request::Body::MetricBatch(batch)),
            ..Default::default()
        };
        self.session.write_only(&request)
    }

    /// The periodic ping plus the overlay lease that keep the session open.
    pub fn keepalive(&mut self, overlay: Option<&OverlayConfig>) -> Result<(), KanaliError> {
        if self.product.idle_mode() == tryx_device::product::IdleMode::TransferOnly {
            return Ok(());
        }
        let ping = wire::Request {
            header: Some(wire::WireHeader::default()),
            body: Some(request::Body::Ping(wire::Ping {
                payload: b"hello?".to_vec(),
            })),
        };
        self.session.write_only(&ping)?;
        let lease = wire::Request {
            header: Some(wire::WireHeader::default()),
            body: Some(request::Body::OverlayLayout(
                overlay.map(overlay::run_config).unwrap_or_default(),
            )),
        };
        self.session.write_only(&lease)?;
        self.session.drain(Duration::from_millis(100));
        Ok(())
    }

    /// The device file name for prepared media: the local name with its
    /// original extension plus the product's geometry suffix.
    pub fn remote_name(&self, local_name: &str) -> String {
        format!("{local_name}{}", self.product.media_name_suffix())
    }

    /// Uploads a prepared file with begin, chunk, and end acknowledgements.
    pub fn upload(
        &mut self,
        path: &Path,
        remote_name: &str,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<(), KanaliError> {
        self.require(self.product.capabilities().media_upload, "media upload")?;
        if !is_safe_device_name(remote_name)
            || !remote_name.ends_with(self.product.media_name_suffix())
        {
            return Err(KanaliError::Invalid(format!(
                "remote name {remote_name:?} must use the device charset and end with {}",
                self.product.media_name_suffix()
            )));
        }
        let size = std::fs::metadata(path)?.len();
        if size == 0 || size > MAX_UPLOAD_BYTES || u32::try_from(size).is_err() {
            return Err(KanaliError::Invalid(format!(
                "{} bytes is outside the accepted upload size",
                size
            )));
        }
        let track = (self.product == Product::Turris620).then_some(TURRIS_TRANSFER_TRACK_ID);
        let mut file = std::fs::File::open(path)?;

        let mut begin = wire::Request {
            body: Some(request::Body::TransferBegin(wire::TransferBegin {
                file_name: remote_name.as_bytes().to_vec(),
                file_size: size as u32,
            })),
            ..Default::default()
        };
        let response = self.session.execute(
            &mut begin,
            Expect::TransferBeginStatus,
            Profile::FileTransmit,
            track,
        )?;
        check_transfer(&response, "transfer_begin")?;

        use std::io::Read;
        let mut sent = 0u64;
        let mut chunk = vec![0u8; CHUNK_SIZE];
        while sent < size {
            let wanted = (size - sent).min(CHUNK_SIZE as u64) as usize;
            file.read_exact(&mut chunk[..wanted])?;
            let mut data = wire::Request {
                body: Some(request::Body::TransferChunk(wire::TransferChunk {
                    file_data: chunk[..wanted].to_vec(),
                })),
                ..Default::default()
            };
            let response = self.session.execute(
                &mut data,
                Expect::TransferChunkStatus,
                Profile::FileTransmit,
                track,
            )?;
            check_transfer(&response, "transfer_chunk")?;
            sent += wanted as u64;
            progress(sent, size);
        }

        let mut end = wire::Request {
            body: Some(request::Body::TransferEnd(wire::TransferEnd {
                file_type: b"media".to_vec(),
                checksum: 0,
            })),
            ..Default::default()
        };
        let response = self.session.execute(
            &mut end,
            Expect::TransferEndStatus,
            Profile::FileTransmit,
            track,
        )?;
        check_transfer(&response, "transfer_end")?;
        Ok(())
    }

    pub fn delete(&mut self, remote_name: &str) -> Result<(), KanaliError> {
        self.require(self.product.capabilities().media_catalog, "media deletion")?;
        let mut request = wire::Request {
            body: Some(request::Body::FileRemoval(wire::FileRemoval {
                file_name: remote_name.to_string(),
                file_type: "media".to_string(),
            })),
            ..Default::default()
        };
        self.session
            .execute(&mut request, Expect::Acknowledgement, Profile::Normal, None)?;
        Ok(())
    }
}

fn check_transfer(response: &wire::Response, request: &'static str) -> Result<(), KanaliError> {
    let status = match &response.body {
        Some(response::Body::TransferBeginStatus(s))
        | Some(response::Body::TransferChunkStatus(s))
        | Some(response::Body::TransferEndStatus(s)) => s.status,
        _ => {
            return Err(KanaliError::InvalidResponse {
                request,
                detail: "no transfer status".into(),
            });
        }
    };
    use wire::transfer_status::Code;
    match Code::try_from(status) {
        Ok(Code::Ok) => Ok(()),
        Ok(Code::SpaceNotEnough) => Err(KanaliError::Transfer(
            "not enough space on the device".into(),
        )),
        Ok(Code::FileError) => Err(KanaliError::Transfer(
            "the device could not write the file".into(),
        )),
        Ok(Code::ChecksumFailure) => Err(KanaliError::Transfer("checksum failure".into())),
        Err(_) => Err(KanaliError::Transfer(format!("unknown status {status}"))),
    }
}

fn state_from(config: &wire::UserConfiguration) -> Option<DisplayState> {
    let display = config.display_config.as_ref()?;
    let work = config.work_config.as_ref()?;
    use wire::work_configuration::{LoopMode, MediaMode};
    let (screen_mode, play_mode, media) =
        match MediaMode::try_from(work.media_mode).unwrap_or(MediaMode::MediaSingle) {
            MediaMode::MediaDual => (
                "Screen Splitting",
                "Single",
                vec![
                    work.dual_mode_left_media_file.clone(),
                    work.dual_mode_right_media_file.clone(),
                ],
            ),
            MediaMode::MediaKaleidoscope => (
                "Kaleidoscope",
                "Single",
                vec![work.kaleidoscope_media_file.clone()],
            ),
            MediaMode::MediaSingle => (
                "Full Screen",
                match LoopMode::try_from(work.loop_mode).unwrap_or(LoopMode::LoopSingle) {
                    LoopMode::LoopAll => "Loop",
                    LoopMode::LoopRandom => "Shuffle",
                    LoopMode::LoopSingle => "Single",
                },
                vec![work.single_mode_media_file.clone()],
            ),
        };
    Some(DisplayState {
        backlight_enabled: display.backlight_enable,
        brightness: display.backlight_brightness.min(100),
        standby_enabled: config
            .standby_config
            .as_ref()
            .map(|s| s.enable)
            .unwrap_or(false),
        standby_media: config
            .standby_config
            .as_ref()
            .map(|s| s.media_file.clone())
            .unwrap_or_default(),
        mirror: display.media_rotation == 180,
        waterfall: display.ui_rotation == 90,
        screen_mode: screen_mode.to_string(),
        play_mode: play_mode.to_string(),
        media,
    })
}

/// Names the firmware accepts: at most 128 chars of `[A-Za-z0-9._-]`, no
/// leading dot, no separators.
pub fn is_safe_device_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Serialises a request into a frame.
pub fn encode_request(request: &wire::Request) -> Result<Vec<u8>, KanaliError> {
    let payload = request.encode_to_vec();
    tryx_proto::frame::encode(&payload)
        .ok_or_else(|| KanaliError::Invalid("request larger than 1 MiB".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_follow_the_firmware_rules() {
        assert!(is_safe_device_name("clip.mp4.h264_2240x1080"));
        assert!(!is_safe_device_name(".hidden"));
        assert!(!is_safe_device_name("a/b"));
        assert!(!is_safe_device_name("sp ace"));
        assert!(!is_safe_device_name(&"x".repeat(129)));
    }

    #[test]
    fn state_maps_work_and_display_configuration() {
        let config = wire::UserConfiguration {
            display_config: Some(wire::DisplayConfiguration {
                backlight_enable: true,
                backlight_brightness: 250,
                ui_rotation: 90,
                media_rotation: 180,
                ..Default::default()
            }),
            work_config: Some(wire::WorkConfiguration {
                media_mode: wire::work_configuration::MediaMode::MediaSingle as i32,
                loop_mode: wire::work_configuration::LoopMode::LoopAll as i32,
                single_mode_media_file: "a.mp4.h264_2240x1080".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let state = state_from(&config).unwrap();
        assert_eq!(state.brightness, 100, "clamped");
        assert!(state.waterfall && state.mirror);
        assert_eq!(state.screen_mode, "Full Screen");
        assert_eq!(state.play_mode, "Loop");
        assert_eq!(state.media, vec!["a.mp4.h264_2240x1080"]);
        assert!(state_from(&wire::UserConfiguration::default()).is_none());
    }
}
