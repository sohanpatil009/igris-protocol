use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EcosystemConfig {
    pub enabled: bool,
    pub port: u16,
    pub auto_discovery: bool,
    pub clipboard_sync: bool,
    pub notification_sync: bool,
    pub device_name: String,
    pub storage_dir: PathBuf,
}

impl Default for EcosystemConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: crate::eco::constants::DEFAULT_ECO_PORT,
            auto_discovery: true,
            clipboard_sync: false,
            notification_sync: false,
            device_name: whoami::fallible::hostname().unwrap_or_default(),
            storage_dir: PathBuf::from("pkg").join(crate::eco::constants::ECO_STORAGE_DIR),
        }
    }
}

impl EcosystemConfig {
    pub fn from_file(path: &PathBuf) -> Option<Self> {
        if !path.exists() {
            return None;
        }
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn save(&self, path: &PathBuf) {
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, &data);
        }
    }
}
