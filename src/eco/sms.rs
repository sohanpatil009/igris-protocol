use crate::eco::events::{EcoEvent, EventBus};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Enterprise SMS record (Igris native).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SmsData {
    pub sms_id: String,
    pub thread_id: String,
    pub address: String,
    pub body: String,
    pub date: i64,
    pub read: bool,
    pub device_name: String,
    pub device_id: String,
    pub timestamp: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SmsStore {
    pub messages: Vec<SmsData>,
}

impl SmsStore {
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

pub struct SmsManager {
    event_bus: Arc<EventBus>,
    storage_path: PathBuf,
    store: SmsStore,
    local_device_id: String,
    local_device_name: String,
}

impl SmsManager {
    pub fn new(
        event_bus: Arc<EventBus>,
        pkg_dir: &PathBuf,
        local_device_id: String,
        local_device_name: String,
    ) -> Self {
        let storage_path = pkg_dir
            .join(crate::eco::constants::ECO_STORAGE_DIR)
            .join("sms_history.json");
        let store = SmsStore::load(&storage_path);
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
        sms_id: String,
        thread_id: String,
        address: String,
        body: String,
    ) -> SmsData {
        let data = SmsData {
            sms_id,
            thread_id,
            address,
            body,
            date: chrono::Utc::now().timestamp_millis(),
            read: false,
            device_name: self.local_device_name.clone(),
            device_id: self.local_device_id.clone(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        self.store.messages.insert(0, data.clone());
        if self.store.messages.len() > 200 {
            self.store.messages.truncate(200);
        }
        self.store.save(&self.storage_path);
        self.event_bus.emit(EcoEvent::SmsReceived(data.clone()));
        data
    }

    pub fn receive_remote(&mut self, data: SmsData) {
        if self.store.messages.iter().any(|m| m.sms_id == data.sms_id) {
            return;
        }
        self.store.messages.insert(0, data);
        if self.store.messages.len() > 200 {
            self.store.messages.truncate(200);
        }
        self.store.save(&self.storage_path);
    }

    pub fn get_messages(&self) -> Vec<SmsData> {
        self.store.messages.clone()
    }

    pub fn set_device_info(&mut self, id: String, name: String) {
        self.local_device_id = id;
        self.local_device_name = name;
    }
}

lazy_static::lazy_static! {
    pub static ref SMS_HISTORY: std::sync::Mutex<Option<Vec<SmsData>>> =
        std::sync::Mutex::new(None);
}

pub fn get_sms() -> Vec<SmsData> {
    SMS_HISTORY.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

pub fn set_sms(messages: Vec<SmsData>) {
    if let Ok(mut guard) = SMS_HISTORY.lock() {
        *guard = Some(messages);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_media_payload_backward_compat() {
        let json = serde_json::json!({
            "content_hash": "h",
            "content": "hello",
            "content_type": "text/plain",
            "source_device": "d1",
            "timestamp": 1
        });
        let p: crate::eco::protocol::ClipboardSyncPayload = serde_json::from_value(json).unwrap();
        assert_eq!(p.content, "hello");
        assert!(p.encoding.is_empty());
        assert_eq!(p.byte_size, 0);
    }

    #[test]
    fn test_clipboard_media_payload_full() {
        let json = serde_json::json!({
            "content_hash": "h",
            "content": "aGVsbG8=",
            "content_type": "image/png",
            "source_device": "d1",
            "timestamp": 1,
            "encoding": "base64",
            "file_name": "img.png",
            "mime_type": "image/png",
            "byte_size": 5
        });
        let p: crate::eco::protocol::ClipboardSyncPayload = serde_json::from_value(json).unwrap();
        assert_eq!(p.encoding, "base64");
        assert_eq!(p.mime_type, "image/png");
    }
}
