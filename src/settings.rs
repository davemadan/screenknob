//! Remembered choices, kept per output by its UID so a screen that comes back
//! after sleeping gets its own volume again.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceVolume {
    pub volume: f32,
    pub muted: bool,
}

impl Default for DeviceVolume {
    fn default() -> Self {
        Self { volume: 0.5, muted: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The output you picked last; restored when it comes back.
    pub preferred_uid: Option<String>,
    pub auto_return: bool,
    pub software_volume: bool,
    pub volume_keys: bool,
    /// Route screens through ScreenKnob's virtual speaker so macOS shows its own
    /// volume popup and click, as for a real speaker.
    pub native_popup: bool,
    /// Opening at login was set up once already (it's on by default when installed).
    pub login_set_up: bool,
    /// Set the screen's own speaker volume over its cable when it supports it
    /// (no sound is captured, so no recording indicator).
    pub monitor_volume: bool,
    pub volumes: HashMap<String, DeviceVolume>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preferred_uid: None,
            auto_return: true,
            software_volume: true,
            volume_keys: true,
            native_popup: true,
            login_set_up: false,
            monitor_volume: true,
            volumes: HashMap::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("ScreenKnob").join("settings.json"))
}

/// Where settings lived under the app's earlier name.
fn old_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Monitor Volume").join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .or_else(|| old_path().and_then(|p| std::fs::read_to_string(p).ok()))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }

    pub fn volume_for(&self, uid: &str) -> DeviceVolume {
        self.volumes.get(uid).cloned().unwrap_or_default()
    }
}
