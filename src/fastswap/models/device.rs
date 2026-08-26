use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Device {
    pub id: String,
    pub alias: String,
    pub device_model: String,
    pub device_type: DeviceType,
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    pub download: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Mobile,
    Desktop,
    Web,
    Headless,
}

impl Device {
    pub fn new_local(alias: String, port: u16, ip: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            alias,
            device_model: whoami::devicename(),
            device_type: DeviceType::Desktop,
            ip,
            port,
            protocol: "2.0".to_string(),
            download: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub alias: String,
    pub version: String,
    #[serde(rename = "deviceModel")]
    pub device_model: String,
    #[serde(rename = "deviceType")]
    pub device_type: DeviceType,
    pub fingerprint: String,
    pub port: u16,
    pub protocol: String,
    pub download: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub alias: String,
    pub version: String,
    #[serde(rename = "deviceModel")]
    pub device_model: String,
    #[serde(rename = "deviceType")]
    pub device_type: DeviceType,
    pub fingerprint: String,
}
