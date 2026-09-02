use crate::eco::errors::{EcoError, EcoResult};
use crate::eco::storage::EcoStorage;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use uuid::Uuid;

const OTP_EXPIRY_SECS: u64 = 120;
const OTP_LENGTH: usize = 6;

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn generate_otp_code() -> String {
    let mut rng = rand::thread_rng();
    let otp: u32 = rng.gen_range(100_000..1_000_000);
    format!("{:0width$}", otp, width = OTP_LENGTH)
}

pub fn hash_otp_code(otp: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(otp.as_bytes());
    hex_encode(&hasher.finalize())
}

#[derive(Clone, Debug)]
pub struct PendingPairingRequest {
    pub id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub sender_addr: SocketAddr,
    pub otp_hash: String,
    pub received_at: Instant,
}

#[derive(Clone, Debug)]
pub struct ActivePairingSession {
    pub device_id: String,
    pub otp: String,
    pub otp_hash: String,
    pub created_at: Instant,
}

#[derive(Clone, Debug)]
pub struct PairingRequest {
    pub sender_id: String,
    pub sender_name: String,
    pub sender_addr: SocketAddr,
    pub otp_hash: String,
}

pub struct PairingManager {
    pending_requests: Arc<RwLock<Vec<PendingPairingRequest>>>,
    active_sessions: Arc<RwLock<HashMap<String, ActivePairingSession>>>,
    storage: Arc<std::sync::Mutex<EcoStorage>>,
}

impl PairingManager {
    pub fn new(storage: Arc<std::sync::Mutex<EcoStorage>>) -> Self {
        Self {
            pending_requests: Arc::new(RwLock::new(Vec::new())),
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
            storage,
        }
    }

    pub fn get_pending_requests(&self) -> Arc<RwLock<Vec<PendingPairingRequest>>> {
        self.pending_requests.clone()
    }

    pub fn get_active_sessions(&self) -> Arc<RwLock<HashMap<String, ActivePairingSession>>> {
        self.active_sessions.clone()
    }

    pub fn get_trusted_ids(&self) -> Vec<String> {
        self.storage.lock().map(|s| s.get_trusted_device_ids().clone()).unwrap_or_default()
    }

    pub fn generate_otp() -> String {
        generate_otp_code()
    }

    pub fn hash_otp(otp: &str) -> String {
        hash_otp_code(otp)
    }

    pub async fn create_pairing_session(&self, device_id: &str) -> EcoResult<String> {
        let otp = Self::generate_otp();
        let otp_hash = Self::hash_otp(&otp);

        let session = ActivePairingSession {
            device_id: device_id.to_string(),
            otp: otp.clone(),
            otp_hash,
            created_at: Instant::now(),
        };

        let mut sessions = self.active_sessions.write().await;
        sessions.insert(device_id.to_string(), session);
        Ok(otp)
    }

    pub async fn get_active_otp(&self, device_id: &str) -> Option<String> {
        let sessions = self.active_sessions.read().await;
        sessions.get(device_id).map(|s| s.otp.clone())
    }

    pub async fn add_pending_request(
        &self,
        sender_id: String,
        sender_name: String,
        sender_addr: SocketAddr,
        otp_hash: String,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        let req = PendingPairingRequest {
            id: id.clone(),
            sender_id,
            sender_name,
            sender_addr,
            otp_hash,
            received_at: Instant::now(),
        };
        let mut requests = self.pending_requests.write().await;
        requests.push(req);
        id
    }

    pub async fn verify_and_trust(
        &self,
        pending_id: &str,
        entered_otp: &str,
        local_device_id: &str,
        remote_device_id: &str,
    ) -> EcoResult<bool> {
        let mut requests = self.pending_requests.write().await;
        let pos = requests.iter().position(|r| r.id == pending_id);
        if let Some(idx) = pos {
            let req = requests.remove(idx);
            let entered_hash = Self::hash_otp(entered_otp);
            if entered_hash == req.otp_hash {
                let mut storage = self.storage.lock().map_err(|_| {
                    EcoError::Storage("Lock failed".to_string())
                })?;
                storage.trust_device(remote_device_id).ok();
                storage.trust_device(local_device_id).ok();
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Err(EcoError::PairingFailed("Pending request not found".to_string()))
        }
    }

    pub async fn trust_device_direct(&self, device_id: &str) -> EcoResult<()> {
        let mut storage = self.storage.lock().map_err(|_| {
            EcoError::Storage("Lock failed".to_string())
        })?;
        storage.trust_device(device_id)
    }

    pub async fn remove_trust(&self, device_id: &str) -> EcoResult<()> {
        let mut storage = self.storage.lock().map_err(|_| {
            EcoError::Storage("Lock failed".to_string())
        })?;
        storage.untrust_device(device_id)
    }

    pub async fn is_trusted(&self, device_id: &str) -> bool {
        let storage = self.storage.lock().unwrap();
        storage.is_device_trusted(device_id)
    }

    pub async fn cleanup_expired(&self) {
        let mut requests = self.pending_requests.write().await;
        let now = Instant::now();
        requests.retain(|r| now.duration_since(r.received_at).as_secs() < OTP_EXPIRY_SECS);

        let mut sessions = self.active_sessions.write().await;
        sessions.retain(|_, s| now.duration_since(s.created_at).as_secs() < OTP_EXPIRY_SECS);
    }
}

lazy_static::lazy_static! {
    pub static ref PAIRING_MANAGER: std::sync::Mutex<Option<PairingManager>> = std::sync::Mutex::new(None);
}

pub fn init_pairing_manager(storage: Arc<std::sync::Mutex<EcoStorage>>) {
    let manager = PairingManager::new(storage);
    let mut guard = PAIRING_MANAGER.lock().unwrap();
    *guard = Some(manager);
}

pub fn get_pairing_manager() -> Option<std::sync::MutexGuard<'static, Option<PairingManager>>> {
    PAIRING_MANAGER.lock().ok()
}

/// Synchronous persistence helpers used from server handlers where holding the
/// `PAIRING_MANAGER` guard across an `.await` is undesirable. All storage writes
/// are quick std::fs operations.
pub fn persist_trust(device_id: &str) {
    if let Some(guard) = get_pairing_manager() {
        if let Some(manager) = guard.as_ref() {
            if let Ok(mut storage) = manager.storage.lock() {
                storage.trust_device(device_id).ok();
            }
        }
    }
}

pub fn persist_untrust(device_id: &str) {
    if let Some(guard) = get_pairing_manager() {
        if let Some(manager) = guard.as_ref() {
            if let Ok(mut storage) = manager.storage.lock() {
                storage.untrust_device(device_id).ok();
            }
        }
    }
}

pub fn is_trusted_sync(device_id: &str) -> bool {
    if let Some(guard) = get_pairing_manager() {
        if let Some(manager) = guard.as_ref() {
            return manager
                .storage
                .lock()
                .map(|s| s.is_device_trusted(device_id))
                .unwrap_or(false);
        }
    }
    false
}

/// Count of persisted trusted device ids. Read from storage so it is correct
/// at boot (before any live discovery has run) and survives restarts. Used by
/// the Android keep-alive tier to decide whether to hold the service alive.
pub fn trusted_count() -> u32 {
    if let Some(guard) = get_pairing_manager() {
        if let Some(manager) = guard.as_ref() {
            return manager
                .storage
                .lock()
                .map(|s| s.get_trusted_device_ids().len() as u32)
                .unwrap_or(0);
        }
    }
    0
}

lazy_static::lazy_static! {
    static ref LOCAL_DEVICE_INFO: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);
}

pub fn set_local_device_info(id: String, name: String) {
    if let Ok(mut info) = LOCAL_DEVICE_INFO.lock() {
        *info = Some((id, name));
    }
}

pub fn get_local_device_id() -> Option<String> {
    LOCAL_DEVICE_INFO.lock().ok()
        .and_then(|info| info.as_ref().map(|(id, _)| id.clone()))
}

pub fn get_local_device_name() -> Option<String> {
    LOCAL_DEVICE_INFO.lock().ok()
        .and_then(|info| info.as_ref().map(|(_, name)| name.clone()))
}
