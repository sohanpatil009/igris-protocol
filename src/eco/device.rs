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
    #[serde(default)]
    pub call_sync: bool,
    #[serde(default)]
    pub sms_sync: bool,
    #[serde(default)]
    pub clipboard_media: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            clipboard_sync: true,
            notification_sync: false,
            remote_commands: false,
            call_sync: false,
            sms_sync: false,
            clipboard_media: false,
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
    /// Peer's self-reported list of trusted device IDs (from `/info`).
    /// `None` = peer is an old binary that doesn't report this field.
    /// `Some([])` = peer reports it has no trusted peers (stale trust).
    #[serde(default)]
    pub trusted_peers: Option<Vec<String>>,
    /// Peer's advertised FastSwap native TLS port, from `/info`
    /// `fastswap_port`. 0 = peer is an old binary that didn't report it;
    /// callers should fall back to FASTSWAP_TLS_PORT (53318).
    #[serde(default)]
    pub fastswap_port: u16,
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
            trusted_peers: None,
            fastswap_port: 0,
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
