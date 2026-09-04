//! What the display was last told, kept under the XDG state directory so
//! commands can change one setting without resetting the others.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tryx_legacy::ScreenConfig;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayState {
    pub screen: ScreenConfig,
    pub brightness: Option<u8>,
    pub cpu_name: Option<String>,
    pub gpu_name: Option<String>,
    pub temperature_unit: Option<String>,
    /// Fixed display-block fan speed last set, re-applied by the daemon.
    pub fan_lcd_percent: Option<u8>,
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("tryxctl/display.json"))
}

/// The saved state, or the default when there is none or it is unreadable.
pub fn load() -> DisplayState {
    path().map(|p| load_from(&p)).unwrap_or_default()
}

pub fn load_from(path: &Path) -> DisplayState {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(state: &DisplayState) -> std::io::Result<()> {
    match path() {
        Some(p) => save_to(&p, state),
        None => Ok(()),
    }
}

pub fn save_to(path: &Path, state: &DisplayState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_or_partial_files() {
        let dir = std::env::temp_dir().join(format!("tryxctl-state-{}", std::process::id()));
        let file = dir.join("nested/display.json");
        assert_eq!(load_from(&file), DisplayState::default());
        let state = DisplayState {
            screen: ScreenConfig {
                media: vec!["a.mp4".into()],
                sysinfo_display: vec!["CPU Temperature".into()],
                ..ScreenConfig::default()
            },
            brightness: Some(75),
            cpu_name: Some("Ryzen".into()),
            gpu_name: None,
            temperature_unit: None,
            fan_lcd_percent: None,
        };
        save_to(&file, &state).unwrap();
        assert_eq!(load_from(&file), state);
        std::fs::write(&file, r#"{"brightness": 40}"#).unwrap();
        let partial = load_from(&file);
        assert_eq!(partial.brightness, Some(40));
        assert_eq!(partial.screen, ScreenConfig::default());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
