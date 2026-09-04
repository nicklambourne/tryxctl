//! JSON bodies of the legacy commands.
//!
//! Bodies are built as `serde_json` maps, which serialise with sorted keys
//! like the vendor app's `std::map`-backed JSON, so payloads match it byte
//! for byte apart from number formatting.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// `Top`, `Center`, or `Bottom`.
    pub position: String,
    /// `#RRGGBB`.
    pub color: String,
    /// `Left`, `Center`, or `Right`.
    pub align: String,
    /// `CPU Badge`, `GPU Badge`.
    pub badges: Vec<String>,
    /// Overlay filter opacity, 0 to 100.
    pub filter_opacity: u32,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        DisplaySettings {
            position: "Top".to_string(),
            color: "#FFFFFF".to_string(),
            align: "Left".to_string(),
            badges: Vec::new(),
            filter_opacity: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenConfig {
    /// Factory preset such as `Pre-set 1: Cooling delivery`; empty selects
    /// the custom `media` list instead.
    pub preset_id: String,
    /// File names under the device media directory.
    pub media: Vec<String>,
    /// `Full Screen` or `Screen Splitting`.
    pub screen_mode: String,
    pub ratio: String,
    /// `Single`, `Loop`, or `Shuffle`.
    pub play_mode: String,
    /// Up to three metric labels shown on the (left) overlay.
    pub sysinfo_display: Vec<String>,
    pub settings: DisplaySettings,
    /// Right half, used only in `Screen Splitting` mode.
    pub settings2: DisplaySettings,
    pub sysinfo_display2: Vec<String>,
    pub waterfall_mode: bool,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        ScreenConfig {
            preset_id: String::new(),
            media: Vec::new(),
            screen_mode: "Full Screen".to_string(),
            ratio: "2:1".to_string(),
            play_mode: "Single".to_string(),
            sysinfo_display: Vec::new(),
            settings: DisplaySettings::default(),
            settings2: DisplaySettings::default(),
            sysinfo_display2: Vec::new(),
            waterfall_mode: false,
        }
    }
}

pub const SCREEN_SPLITTING: &str = "Screen Splitting";

fn settings_json(settings: &DisplaySettings) -> Value {
    json!({
        "position": settings.position,
        "color": settings.color,
        "align": settings.align,
        "filter": {"value": "", "opacity": settings.filter_opacity},
        "badges": settings.badges,
    })
}

fn insert_layout(body: &mut Map<String, Value>, config: &ScreenConfig) {
    if config.screen_mode == SCREEN_SPLITTING {
        body.insert(
            "settings".into(),
            json!([
                settings_json(&config.settings),
                settings_json(&config.settings2)
            ]),
        );
        body.insert(
            "sysinfoDisplay".into(),
            json!([config.sysinfo_display, config.sysinfo_display2]),
        );
    } else {
        body.insert("settings".into(), settings_json(&config.settings));
        body.insert("sysinfoDisplay".into(), json!(config.sysinfo_display));
    }
}

fn insert_identity(body: &mut Map<String, Value>, config: &ScreenConfig) {
    if config.preset_id.is_empty() {
        body.insert("Type".into(), json!("Custom"));
        body.insert("id".into(), json!("Customization"));
    } else {
        body.insert("Type".into(), json!("Pre-set"));
        body.insert("id".into(), json!(config.preset_id));
    }
    body.insert("screenMode".into(), json!(config.screen_mode));
    body.insert("ratio".into(), json!(config.ratio));
    body.insert("playMode".into(), json!(config.play_mode));
}

/// Body of `POST waterBlockScreenId`.
pub fn screen_config(config: &ScreenConfig) -> Value {
    let mut body = Map::new();
    insert_identity(&mut body, config);
    if config.preset_id.is_empty() {
        body.insert("media".into(), json!(config.media));
    }
    insert_layout(&mut body, config);
    Value::Object(body)
}

/// Body of `POST config`, the combined configuration in the vendor app's
/// format.
pub fn full_config(
    config: &ScreenConfig,
    cpu: &str,
    gpu: &str,
    brightness: u8,
    temperature_unit: &str,
) -> Value {
    let mut screen = Map::new();
    insert_identity(&mut screen, config);
    screen.insert("media".into(), json!(config.media));
    insert_layout(&mut screen, config);
    json!({
        "temperature": temperature_unit,
        "waterBlockScreen": {
            "enable": true,
            "displayInSleep": false,
            "brightness": brightness,
            "waterfallMode": config.waterfall_mode,
            "id": Value::Object(screen),
        },
        "spec": {"cpu": cpu, "gpu": gpu},
    })
}

/// Body of `POST sysinfoDisplay`.
pub fn sysinfo_display(labels: &[String]) -> Value {
    json!({"items": labels})
}

/// Body of `POST temperature`: `Celsius` or `Fahrenheit`.
pub fn temperature_unit(unit: &str) -> Value {
    json!({"value": unit})
}

/// Body of `POST spec`: hardware names for the badges.
pub fn spec(cpu: &str, gpu: &str) -> Value {
    json!({"cpu": cpu, "gpu": gpu})
}

pub fn waterfall_mode(enable: bool) -> Value {
    json!({"enable": enable})
}

pub fn rotate(degrees: u16) -> Value {
    json!({"degree": degrees})
}

pub fn brightness(value: u8) -> Value {
    json!({"value": value})
}

pub fn media_delete(files: &[String]) -> Value {
    json!({"include": files})
}

/// Live metrics for `POST all`, in the shape the firmware expects.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PcInfo {
    pub cpu: CpuInfo,
    pub gpu: GpuInfo,
    pub memory: MemoryInfo,
    pub motherboard_temperature: f64,
    pub disk: DiskInfo,
    pub network: NetworkInfo,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuInfo {
    pub load: f64,
    pub temperature: f64,
    pub speed_average: f64,
    pub voltage: f64,
    pub power: f64,
    pub fan_average: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GpuInfo {
    pub load: f64,
    /// The firmware reads this field as a string.
    pub temperature: String,
    pub speed: f64,
    pub voltage: f64,
    pub power: f64,
    pub fan: f64,
}

impl Default for GpuInfo {
    fn default() -> Self {
        GpuInfo {
            load: 0.0,
            temperature: "0".to_string(),
            speed: 0.0,
            voltage: 0.0,
            power: 0.0,
            fan: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryInfo {
    pub load: f64,
    pub speed: f64,
    pub temperature: f64,
    pub total: f64,
    pub used: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiskInfo {
    pub load: f64,
    pub used: f64,
    pub total: f64,
    pub temperature: f64,
    pub activity: f64,
    pub read_speed: f64,
    pub write_speed: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkInfo {
    pub download: f64,
    pub upload: f64,
}

pub fn pc_info(info: &PcInfo) -> Value {
    json!({
        "cpu": {
            "load": info.cpu.load,
            "temperature": info.cpu.temperature,
            "speedAverage": info.cpu.speed_average,
            "voltage": info.cpu.voltage,
            "power": info.cpu.power,
            "fanAverage": info.cpu.fan_average,
        },
        "gpu": {
            "load": info.gpu.load,
            "temperature": info.gpu.temperature,
            "speed": info.gpu.speed,
            "voltage": info.gpu.voltage,
            "power": info.gpu.power,
            "fan": info.gpu.fan,
        },
        "memory": {
            "load": info.memory.load,
            "speed": info.memory.speed,
            "temperature": info.memory.temperature,
            "total": info.memory.total,
            "used": info.memory.used,
        },
        "motherboard": {"temperature": info.motherboard_temperature},
        "disk": {
            "load": info.disk.load,
            "used": info.disk.used,
            "total": info.disk.total,
            "temperature": info.disk.temperature,
            "activity": info.disk.activity,
            "readSpeed": info.disk.read_speed,
            "writeSpeed": info.disk.write_speed,
        },
        "network": {"download": info.network.download, "upload": info.network.upload},
        "fans": [],
        "timestamp": info.timestamp_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> ScreenConfig {
        ScreenConfig {
            media: vec!["a.mp4".into()],
            sysinfo_display: vec!["CPU Temperature".into()],
            ..ScreenConfig::default()
        }
    }

    #[test]
    fn keys_are_sorted_like_the_vendor_app() {
        let text = screen_config(&custom()).to_string();
        assert!(
            text.starts_with(r##"{"Type":"Custom","id":"Customization","media":["a.mp4"],"playMode":"Single","ratio":"2:1","screenMode":"Full Screen","settings":{"align":"Left","badges":[],"color":"#FFFFFF","filter":{"opacity":0,"value":""},"position":"Top"},"sysinfoDisplay":["CPU Temperature"]}"##),
            "{text}"
        );
    }

    #[test]
    fn presets_omit_the_media_list() {
        let body = screen_config(&ScreenConfig {
            preset_id: "Pre-set 1: Cooling delivery".into(),
            ..custom()
        });
        assert_eq!(body["Type"], "Pre-set");
        assert_eq!(body["id"], "Pre-set 1: Cooling delivery");
        assert!(body.get("media").is_none());
    }

    #[test]
    fn screen_splitting_sends_two_settings_and_two_label_lists() {
        let body = screen_config(&ScreenConfig {
            screen_mode: SCREEN_SPLITTING.into(),
            sysinfo_display2: vec!["GPU Usage".into()],
            ..custom()
        });
        assert_eq!(body["settings"].as_array().unwrap().len(), 2);
        assert_eq!(
            body["sysinfoDisplay"],
            json!([["CPU Temperature"], ["GPU Usage"]])
        );
    }

    #[test]
    fn full_config_nests_the_screen_config_and_always_lists_media() {
        let body = full_config(
            &ScreenConfig {
                preset_id: "Pre-set 2: Ocean".into(),
                ..custom()
            },
            "Ryzen",
            "Radeon",
            60,
            "Celsius",
        );
        assert_eq!(body["temperature"], "Celsius");
        assert_eq!(body["spec"], json!({"cpu": "Ryzen", "gpu": "Radeon"}));
        let screen = &body["waterBlockScreen"];
        assert_eq!(screen["enable"], true);
        assert_eq!(screen["displayInSleep"], false);
        assert_eq!(screen["brightness"], 60);
        assert_eq!(screen["id"]["Type"], "Pre-set");
        assert_eq!(screen["id"]["media"], json!(["a.mp4"]));
    }

    #[test]
    fn small_bodies_match_upstream() {
        assert_eq!(brightness(80).to_string(), r#"{"value":80}"#);
        assert_eq!(rotate(180).to_string(), r#"{"degree":180}"#);
        assert_eq!(waterfall_mode(true).to_string(), r#"{"enable":true}"#);
        assert_eq!(
            temperature_unit("Celsius").to_string(),
            r#"{"value":"Celsius"}"#
        );
        assert_eq!(spec("c", "g").to_string(), r#"{"cpu":"c","gpu":"g"}"#);
        assert_eq!(
            media_delete(&["x.mp4".into(), "y.png".into()]).to_string(),
            r#"{"include":["x.mp4","y.png"]}"#
        );
        assert_eq!(
            sysinfo_display(&["CPU Usage".into()]).to_string(),
            r#"{"items":["CPU Usage"]}"#
        );
    }

    #[test]
    fn pc_info_keeps_gpu_temperature_as_a_string() {
        let info = PcInfo {
            gpu: GpuInfo {
                temperature: "61".into(),
                ..GpuInfo::default()
            },
            timestamp_ms: 1_700_000_000_000,
            ..PcInfo::default()
        };
        let body = pc_info(&info);
        assert_eq!(body["gpu"]["temperature"], "61");
        assert_eq!(body["cpu"]["temperature"], 0.0);
        assert_eq!(body["fans"], json!([]));
        assert_eq!(body["timestamp"], 1_700_000_000_000_i64);
    }
}
