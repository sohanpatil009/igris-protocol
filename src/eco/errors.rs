use std::fmt;

#[derive(Debug)]
pub enum EcoError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Transport(String),
    Discovery(String),
    Clipboard(String),
    Notification(String),
    Crypto(String),
    Storage(String),
    DeviceNotFound,
    PairingFailed(String),
    NotTrusted,
    NotInitialized,
    Timeout,
}

impl fmt::Display for EcoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EcoError::Io(e) => write!(f, "IO error: {}", e),
            EcoError::Serde(e) => write!(f, "Serialization error: {}", e),
            EcoError::Transport(e) => write!(f, "Transport error: {}", e),
            EcoError::Discovery(e) => write!(f, "Discovery error: {}", e),
            EcoError::Clipboard(e) => write!(f, "Clipboard error: {}", e),
            EcoError::Notification(e) => write!(f, "Notification error: {}", e),
            EcoError::Crypto(e) => write!(f, "Crypto error: {}", e),
            EcoError::Storage(e) => write!(f, "Storage error: {}", e),
            EcoError::DeviceNotFound => write!(f, "Device not found"),
            EcoError::PairingFailed(e) => write!(f, "Pairing failed: {}", e),
            EcoError::NotTrusted => write!(f, "Device is not trusted"),
            EcoError::NotInitialized => write!(f, "Ecosystem not initialized"),
            EcoError::Timeout => write!(f, "Operation timed out"),
        }
    }
}

impl std::error::Error for EcoError {}

impl From<std::io::Error> for EcoError {
    fn from(e: std::io::Error) -> Self { EcoError::Io(e) }
}

impl From<serde_json::Error> for EcoError {
    fn from(e: serde_json::Error) -> Self { EcoError::Serde(e) }
}

pub type EcoResult<T> = Result<T, EcoError>;
