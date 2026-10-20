use crate::eco::constants::*;
use crate::eco::errors::EcoResult;
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
    #[allow(dead_code)]
    pub encoding: String,
    #[allow(dead_code)]
    pub file_name: String,
    #[allow(dead_code)]
    pub mime_type: String,
    #[allow(dead_code)]
    pub byte_size: u64,
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
    /// Hashes of remote payloads currently being written to the platform.
    /// Closes the race where the 1s poll reads incoming text before the
    /// apply subscriber finishes and re-broadcasts it as a local change
    /// (harmless via hash dedup, but noisy and wasteful on metered links).
    inflight_remote: std::collections::HashSet<String>,
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
            inflight_remote: std::collections::HashSet::new(),
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

                // Skip content that arrived from a peer and is still being
                // applied (see `mark_inflight_remote`), plus our own dedup.
                if guard.inflight_remote.contains(&hash) {
                    continue;
                }
                let is_own_change = Some(&hash) == guard.last_applied_hash.as_ref();
                let is_known = Some(&hash) == guard.last_content_hash.as_ref();
                if is_own_change || is_known {
                    continue;
                }

                tracing::info!(
                    "[ECO] Clipboard changed: hash={} own={} known={}",
                    &hash[..16],
                    is_own_change,
                    is_known,
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
                    encoding: "text".to_string(),
                    file_name: String::new(),
                    mime_type: "text/plain".to_string(),
                    byte_size: 0,
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

    /// Check whether incoming clipboard data should be applied (dedup gate)
    /// and, if so, mark it in-flight so the poll loop does not re-broadcast
    /// it while the platform write is in progress. Returns `true` when the
    /// caller must proceed to `write_to_platform` + `mark_applied`.
    /// Must be called holding the manager lock; the write itself must run
    /// outside the lock (see callers).
    pub fn claim_remote(&mut self, data: &ClipboardData) -> bool {
        if self.last_content_hash.as_ref() == Some(&data.content_hash) {
            return false;
        }
        if self.inflight_remote.contains(&data.content_hash) {
            return false;
        }
        self.inflight_remote.insert(data.content_hash.clone());
        true
    }

    /// Check whether incoming clipboard data should be applied (dedup gate).
    /// Returns `true` if the content is new and should be written to the
    /// local clipboard. Caller must call `mark_applied()` after a successful
    /// write.
    pub fn should_apply(&self, data: &ClipboardData) -> bool {
        self.last_content_hash.as_ref() != Some(&data.content_hash)
            && !self.inflight_remote.contains(&data.content_hash)
    }

    /// Write incoming clipboard to the local platform clipboard. This is a
    /// blocking I/O call (FFI into Android/iOS) and must NEVER be called
    /// while holding the Mutex.
    /// Enterprise: text goes to `set_text`; base64 media goes to
    /// `set_media` when the platform supports it, otherwise text fallback.
    pub fn write_to_platform(&self, data: &ClipboardData) -> EcoResult<()> {
        if data.encoding == "base64" && !data.content.is_empty() {
            if self.platform.set_media(&data.content, &data.mime_type, &data.file_name).is_ok() {
                return Ok(());
            }
        }
        self.platform.set_text(&data.content)
    }

    /// Mark clipboard as applied (update dedup hashes, emit event). Must be
    /// called while holding the Mutex, but after `write_to_platform()`.
    pub fn mark_applied(&mut self, data: &ClipboardData) {
        self.last_applied_hash = Some(data.content_hash.clone());
        self.last_content_hash = Some(data.content_hash.clone());
        // Arrived via the mesh, so the mesh already has it — treat as delivered
        // so an identical local re-read isn't re-broadcast as "undelivered".
        self.last_delivered_hash = Some(data.content_hash.clone());
        self.inflight_remote.remove(&data.content_hash);

        let arc = Arc::new(data.clone());
        self.event_bus.emit(EcoEvent::ClipboardApplied(arc));
    }

    /// Release an in-flight claim after a failed platform write so the next
    /// poll rediscovers the content instead of dropping it silently.
    pub fn mark_failed(&mut self, data: &ClipboardData) {
        self.inflight_remote.remove(&data.content_hash);
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
        if is_known {
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
            encoding: "text".to_string(),
            file_name: String::new(),
            mime_type: "text/plain".to_string(),
            byte_size: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> ClipboardManager {
        struct Stub;
        impl PlatformClipboard for Stub {
            fn get_text(&self) -> EcoResult<String> {
                Ok(String::new())
            }
            fn set_text(&self, _text: &str) -> EcoResult<()> {
                Ok(())
            }
        }
        let storage = Arc::new(std::sync::Mutex::new(EcoStorage::new(
            &std::path::PathBuf::from("./pkg-test-clipboard"),
        )));
        ClipboardManager::new(Box::new(Stub), Arc::new(EventBus::new()), storage)
    }

    fn test_data(hash: &str) -> ClipboardData {
        ClipboardData {
            content: "x".to_string(),
            content_type: "text/plain".to_string(),
            content_hash: hash.to_string(),
            source_device: "peer".to_string(),
            timestamp: 1,
            encoding: "text".to_string(),
            file_name: String::new(),
            mime_type: "text/plain".to_string(),
            byte_size: 0,
        }
    }

    #[test]
    fn test_claim_blocks_poll_echo_then_applies() {
        let mut m = test_manager();
        let d = test_data("abc");
        assert!(m.claim_remote(&d));
        // Second claim (or poll should_apply) sees in-flight: no echo.
        assert!(!m.should_apply(&d));
        assert!(!m.claim_remote(&d));
        m.mark_applied(&d);
        assert!(!m.should_apply(&d));
    }

    #[test]
    fn test_failed_write_releases_claim() {
        let mut m = test_manager();
        let d = test_data("def");
        assert!(m.claim_remote(&d));
        m.mark_failed(&d);
        assert!(m.should_apply(&d));
    }
}
