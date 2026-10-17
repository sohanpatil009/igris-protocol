use crate::eco::events::{EcoEvent, EventBus};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;


/// Enterprise call record (Igris native).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallData {
    pub call_id: String,
    pub event: String,
    pub phone_number: String,
    pub contact_name: String,
    pub device_name: String,
    pub device_id: String,
    pub timestamp: i64,
    pub is_cancel: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CallStore {
    pub calls: Vec<CallData>,
}

impl CallStore {
    fn load(path: &PathBuf) -> Self {
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default()
    }
    fn save(&self, path: &PathBuf) {
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if let Ok(j) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, j);
        }
    }
}

pub struct CallManager {
    event_bus: Arc<EventBus>,
    storage_path: PathBuf,
    store: CallStore,
    local_device_id: String,
    local_device_name: String,
}

impl CallManager {
    pub fn new(
        event_bus: Arc<EventBus>,
        pkg_dir: &PathBuf,
        local_device_id: String,
        local_device_name: String,
    ) -> Self {
        let storage_path = pkg_dir
            .join(crate::eco::constants::ECO_STORAGE_DIR)
            .join("call_history.json");
        let store = CallStore::load(&storage_path);
        Self {
            event_bus,
            storage_path,
            store,
            local_device_id,
            local_device_name,
        }
    }

    pub fn push_local(
        &mut self,
        call_id: String,
        event: String,
        phone_number: String,
        contact_name: String,
    ) -> CallData {
        let data = CallData {
            call_id,
            event,
            phone_number,
            contact_name,
            device_name: self.local_device_name.clone(),
            device_id: self.local_device_id.clone(),
            timestamp: chrono::Utc::now().timestamp_millis(),
            is_cancel: false,
        };
        // Supersede any older state of the same call so UI banners clear.
        for c in self.store.calls.iter_mut() {
            if c.call_id == data.call_id {
                c.is_cancel = true;
            }
        }
        self.store.calls.insert(0, data.clone());
        if self.store.calls.len() > 100 {
            self.store.calls.truncate(100);
        }
        self.store.save(&self.storage_path);
        self.event_bus.emit(EcoEvent::CallReceived(data.clone()));
        data
    }

    pub fn receive_remote(&mut self, data: CallData) {
        if self.store.calls.iter().any(|c| c.call_id == data.call_id && c.event == data.event) {
            return;
        }
        // Supersede any older state of the same call so UI banners clear.
        for c in self.store.calls.iter_mut() {
            if c.call_id == data.call_id {
                c.is_cancel = true;
            }
        }
        self.store.calls.insert(0, data);
        if self.store.calls.len() > 100 {
            self.store.calls.truncate(100);
        }
        self.store.save(&self.storage_path);
    }

    pub fn get_calls(&self) -> Vec<CallData> {
        self.store.calls.clone()
    }

    pub fn set_device_info(&mut self, id: String, name: String) {
        self.local_device_id = id;
        self.local_device_name = name;
    }
}

lazy_static::lazy_static! {
    pub static ref CALL_HISTORY: std::sync::Mutex<Option<Vec<CallData>>> =
        std::sync::Mutex::new(None);
}

lazy_static::lazy_static! {
    pub static ref CALL_LOG_HISTORY: std::sync::Mutex<Option<Vec<crate::eco::protocol::CallLogEntry>>> =
        std::sync::Mutex::new(None);
}

pub fn get_call_log() -> Vec<crate::eco::protocol::CallLogEntry> {
    CALL_LOG_HISTORY.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

pub fn set_call_log(entries: Vec<crate::eco::protocol::CallLogEntry>) {
    if let Ok(mut guard) = CALL_LOG_HISTORY.lock() {
        *guard = Some(entries);
    }
}

pub fn get_calls() -> Vec<CallData> {
    CALL_HISTORY.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

pub fn set_calls(calls: Vec<CallData>) {
    if let Ok(mut guard) = CALL_HISTORY.lock() {
        *guard = Some(calls);
    }
}

/// Enterprise call-log manager: mirrors a linked peer's call-history rows
/// into local history so the desktop shows the phone's call log without
/// reading telephony APIs on this device.
pub struct CallLogManager {
    calls: std::sync::Mutex<Vec<crate::eco::protocol::CallLogEntry>>,
}

impl CallLogManager {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Receive a call-log snapshot from a trusted peer. Deduplicates by
    /// `call_id` and merges in. Pure data store: no event fan-out — the
    /// REST surface (`/call/log/list`) is the reader.
    pub fn receive_remote(&self, payload: crate::eco::protocol::CallLogSyncPayload) {
        let mut guard = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        for entry in &payload.calls {
            if !guard.iter().any(|c| c.call_id == entry.call_id) {
                guard.push(entry.clone());
            }
        }
        guard.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        if guard.len() > 500 {
            guard.truncate(500);
        }
    }

    pub fn get_log(&self) -> Vec<crate::eco::protocol::CallLogEntry> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_call_payload_serde_backward_compat() {
        let json = serde_json::json!({
            "call_id": "c1",
            "event": "ringing",
            "source_device_id": "d1",
            "source_device_name": "Phone",
            "timestamp": 1
        });
        let p: crate::eco::protocol::CallSyncPayload = serde_json::from_value(json).unwrap();
        assert_eq!(p.event, "ringing");
        assert!(p.phone_number.is_empty());
    }

    #[test]
    fn test_sms_payload_serde() {
        let json = serde_json::json!({
            "sms_id": "s1",
            "thread_id": "t1",
            "address": "+1",
            "body": "hi",
            "date": 1,
            "source_device_id": "d1",
            "source_device_name": "Phone",
            "timestamp": 1
        });
        let p: crate::eco::protocol::SmsSyncPayload = serde_json::from_value(json).unwrap();
        assert_eq!(p.body, "hi");
    }
}
