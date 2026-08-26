use crate::eco::constants::*;
use crate::eco::errors::{EcoError, EcoResult};
use crate::eco::events::{EcoEvent, EventBus};
use crate::eco::storage::{ClipboardEntry, EcoStorage};
use crate::platform::ecosystem::PlatformClipboard;
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ClipboardData {
    pub content: String,
    pub content_type: String,
    pub content_hash: String,
    pub source_device: String,
    pub timestamp: i64,
}

pub struct ClipboardManager {
    platform: Box<dyn PlatformClipboard>,
    event_bus: Arc<EventBus>,
    storage: Arc<std::sync::Mutex<EcoStorage>>,
    last_content_hash: Option<String>,
    last_applied_hash: Option<String>,
    /// Hash of the last local change that was actually delivered to at least
    /// one linked peer. Used to retry a push that failed while peers were
    /// offline instead of deduping it away forever.
    last_delivered_hash: Option<String>,
}

impl ClipboardManager {
    pub fn new(
        platform: Box<dyn PlatformClipboard>,
        event_bus: Arc<EventBus>,
        storage: Arc<std::sync::Mutex<EcoStorage>>,
    ) -> Self {
        Self {
            platform,
            event_bus,
            storage,
            last_content_hash: None,
            last_applied_hash: None,
            last_delivered_hash: None,
        }
    }

    pub async fn start_monitoring(manager: Arc<std::sync::Mutex<Self>>) {
        let mut interval = tokio::time::interval(
            std::time::Duration::from_secs(CLIPBOARD_POLL_INTERVAL_SECS)
        );
        loop {
            interval.tick().await;
            let event = {
                let mut guard = match manager.lock() {
                    Ok(g) => g,
                    Err(_) => continue,
                };
                let text = match guard.platform.get_text() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if text.is_empty() { continue; }
                let hash = hash_content(&text);

                let is_own_change = Some(&hash) == guard.last_applied_hash.as_ref();
                let is_known = Some(&hash) == guard.last_content_hash.as_ref();
                let is_delivered = guard.last_delivered_hash.as_ref() == Some(&hash);
                if is_own_change || (is_known && is_delivered) {
                    continue;
                }

                tracing::info!(
                    "[ECO] Clipboard changed: hash={} own={} known={} delivered={}",
                    &hash[..16],
                    is_own_change,
                    is_known,
                    is_delivered
                );

                if !is_known {
                    let entry = ClipboardEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        content: text.clone(),
                        content_type: "text/plain".to_string(),
                        source_device: String::new(),
                        timestamp: chrono::Utc::now().timestamp_millis(),
                        content_hash: hash.clone(),
                    };

                    if let Ok(mut storage) = guard.storage.lock() {
                        let _ = storage.add_clipboard_entry(entry);
                    }

                    guard.last_content_hash = Some(hash.clone());
                }

                let data = ClipboardData {
                    content: text,
                    content_type: "text/plain".to_string(),
                    content_hash: hash.clone(),
                    source_device: String::new(),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                };

                let event = EcoEvent::ClipboardChanged(Arc::new(data));
                event
            };
            let bus = manager.lock().ok().map(|g| g.event_bus.clone());
            if let Some(bus) = bus {
                bus.emit(event);
            }
        }
    }

    pub fn apply_clipboard(&mut self, data: &ClipboardData) -> EcoResult<()> {
        if self.last_content_hash.as_ref() == Some(&data.content_hash) {
            return Ok(());
        }

        self.platform.set_text(&data.content)?;
        self.last_applied_hash = Some(data.content_hash.clone());
        self.last_content_hash = Some(data.content_hash.clone());
        // Arrived via the mesh, so the mesh already has it — treat as delivered
        // so an identical local re-read isn't re-broadcast as "undelivered".
        self.last_delivered_hash = Some(data.content_hash.clone());

        let arc = Arc::new(data.clone());
        self.event_bus.emit(EcoEvent::ClipboardApplied(arc));
        Ok(())
    }

    pub fn get_current_hash(&self) -> Option<String> {
        self.last_content_hash.clone()
    }

    /// Feed a clipboard change detected outside the poll loop (e.g. an
    /// accessibility hook that bypasses the focused-app read restriction).
    /// Mirrors the monitor's detection path: dedup, history, fan-out event.
    /// Dedup only applies once the content has been delivered to a peer, so an
    /// identical re-push is retried after a failed fan-out (peer offline).
    pub fn push_local_change(&mut self, text: &str) -> EcoResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        let hash = hash_content(text);
        let is_known = Some(&hash) == self.last_content_hash.as_ref();
        let is_delivered = self.last_delivered_hash.as_ref() == Some(&hash);
        if is_known && is_delivered {
            return Ok(());
        }

        if !is_known {
            let entry = ClipboardEntry {
                id: uuid::Uuid::new_v4().to_string(),
                content: text.to_string(),
                content_type: "text/plain".to_string(),
                source_device: String::new(),
                timestamp: chrono::Utc::now().timestamp_millis(),
                content_hash: hash.clone(),
            };

            if let Ok(mut storage) = self.storage.lock() {
                let _ = storage.add_clipboard_entry(entry);
            }
            self.last_content_hash = Some(hash.clone());
        }

        let data = ClipboardData {
            content: text.to_string(),
            content_type: "text/plain".to_string(),
            content_hash: hash,
            source_device: String::new(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };

        let arc = Arc::new(data);
        self.event_bus.emit(EcoEvent::ClipboardChanged(arc));
        Ok(())
    }

    /// Record that a clipboard payload reached at least one linked peer, so an
    /// identical local change is deduped again (no repeat fan-out).
    pub fn mark_delivered(&mut self, hash: &str) {
        self.last_delivered_hash = Some(hash.to_string());
    }

    /// True when the most recent local clipboard content has been delivered to
    /// at least one linked peer. The a11y hook polls this to decide whether an
    /// undelivered push needs to be retried.
    pub fn is_current_delivered(&self) -> bool {
        match (&self.last_delivered_hash, &self.last_content_hash) {
            (Some(d), Some(c)) => d == c,
            _ => false,
        }
    }
}

pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}
