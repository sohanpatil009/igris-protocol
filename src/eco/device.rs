use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeviceStatus {
    Online,
    Offline,
    Pairing,
    Trusted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub clipboard_sync: bool,
    pub notification_sync: bool,
    pub remote_commands: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            clipboard_sync: true,
            notification_sync: false,
            remote_commands: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EcoDevice {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub hostname: String,
    pub version: String,
    pub capabilities: Capabilities,
    pub status: DeviceStatus,
    pub public_key: Option<String>,
    pub addr: Option<SocketAddr>,
    #[serde(skip)]
    pub last_seen: Option<Instant>,
}

impl EcoDevice {
    pub fn new(name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            platform: std::env::consts::OS.to_string(),
            hostname: whoami::fallible::hostname().unwrap_or_default(),
            version: crate::eco::constants::ECO_PROTOCOL_VERSION.to_string(),
            capabilities: Capabilities::default(),
            status: DeviceStatus::Online,
            public_key: None,
            addr: None,
            last_seen: Some(Instant::now()),
        }
    }

    pub fn is_trusted(&self) -> bool {
        self.status == DeviceStatus::Trusted
    }

    pub fn is_online(&self) -> bool {
        matches!(self.status, DeviceStatus::Online | DeviceStatus::Trusted)
    }

    pub fn mark_offline(&mut self) {
        self.status = DeviceStatus::Offline;
    }

    pub fn mark_trusted(&mut self) {
        self.status = DeviceStatus::Trusted;
    }

    pub fn touch(&mut self) {
        self.last_seen = Some(Instant::now());
    }
}
