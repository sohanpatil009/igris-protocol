use crate::eco::constants::*;
use crate::eco::errors::EcoResult;
use crate::eco::events::{EcoEvent, EventBus};
use crate::eco::protocol::NotificationMessage;
use crate::platform::ecosystem::notifications::PlatformNotification;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// A notification synced from any device in the ecosystem.
///
/// v2 fields are `#[serde(default)]` so history files written by v1 load
/// cleanly (missing fields deserialize to their defaults).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationData {
    pub id: String,
    /// Stable identity from the Android `StatusBarNotification.key`.
    /// Empty for legacy/local polls — `id` is the fallback identity.
    #[serde(default)]
    pub notification_key: String,
    #[serde(default)]
    pub app_package: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    /// Base64 PNG (<=96px) — `None` when the peer already cached it.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub icon_hash: String,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub can_reply: bool,
    #[serde(default)]
    pub messages: Vec<NotificationMessage>,
    pub device_name: String,
    pub device_id: String,
    pub timestamp: i64,
    pub read: bool,
    pub replied: bool,
}

/// Rich notification input captured by the platform listener (Android
/// NotificationListenerService), passed through FFI from Kotlin.
#[derive(Clone, Debug, Default)]
pub struct RichNotification {
    pub key: String,
    pub app_package: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub icon: Option<String>,
    pub icon_hash: String,
    pub actions: Vec<String>,
    pub can_reply: bool,
    pub messages: Vec<NotificationMessage>,
}

/// Outcome of feeding a local notification into the store.
#[derive(Clone, Debug)]
pub struct PushResult {
    pub notification: NotificationData,
    /// True when the key already existed and was updated in place.
    pub is_update: bool,
}

/// Reply to send back through the originating app.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationReply {
    pub notification_id: String,
    #[serde(default)]
    pub notification_key: String,
    pub reply_text: String,
    pub source_device_id: String,
}

/// Persistent notification history stored under pkg/ecosystem/.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct NotificationStore {
    pub notifications: Vec<NotificationData>,
}

impl NotificationStore {
    fn load(path: &PathBuf) -> Self {
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &PathBuf) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Manages local notification polling, storage, and cross-device sync.
pub struct NotificationManager {
    platform: Box<dyn PlatformNotification>,
    event_bus: Arc<EventBus>,
    storage_path: PathBuf,
    store: NotificationStore,
    local_device_id: String,
    local_device_name: String,
    /// Per-peer, per-key last icon hash sent on the wire (hash-dedup:
    /// hash-dedup: the PNG bytes are skipped when the hash is unchanged).
    sent_icon_hashes: std::collections::HashMap<String, String>,
}

impl NotificationManager {
    pub fn new(
        platform: Box<dyn PlatformNotification>,
        event_bus: Arc<EventBus>,
        pkg_dir: &PathBuf,
        local_device_id: String,
        local_device_name: String,
    ) -> Self {
        let storage_path = pkg_dir
            .join(ECO_STORAGE_DIR)
            .join(NOTIFICATION_HISTORY_FILE);
        let store = NotificationStore::load(&storage_path);

        Self {
            platform,
            event_bus,
            storage_path,
            store,
            local_device_id,
            local_device_name,
            sent_icon_hashes: std::collections::HashMap::new(),
        }
    }

    /// Poll local notifications and emit events for new ones.
    pub fn poll_local(&mut self) -> Vec<NotificationData> {
        let raw = match self.platform.read_notifications() {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };

        let mut new_notifications = Vec::new();

        for raw_notif in raw {
            // Deduplicate by checking if we already have this notification
            let already_exists = self.store.notifications.iter().any(|n| {
                n.app_name == raw_notif.app_name
                    && n.title == raw_notif.title
                    && n.body == raw_notif.body
                    && n.timestamp > chrono::Utc::now().timestamp_millis() - 60_000
            });

            if !already_exists {
                let notif = NotificationData {
                    id: raw_notif.id,
                    notification_key: String::new(),
                    app_package: String::new(),
                    app_name: raw_notif.app_name,
                    title: raw_notif.title,
                    body: raw_notif.body,
                    icon: None,
                    icon_hash: String::new(),
                    actions: Vec::new(),
                    can_reply: false,
                    messages: Vec::new(),
                    device_name: self.local_device_name.clone(),
                    device_id: self.local_device_id.clone(),
                    timestamp: raw_notif.timestamp,
                    read: false,
                    replied: false,
                };

                self.store.notifications.insert(0, notif.clone());
                new_notifications.push(notif.clone());

                self.event_bus
                    .emit(EcoEvent::NotificationReceived(notif, self.local_device_id.clone()));
            }
        }

        // Trim history to max
        if self.store.notifications.len() > NOTIFICATION_HISTORY_MAX {
            self.store
                .notifications
                .truncate(NOTIFICATION_HISTORY_MAX);
        }

        self.store.save(&self.storage_path);
        new_notifications
    }

    /// Receive a notification from a remote device. Notifications with the
    /// same `(device_id, notification_key)` identity are updated in place
    /// instead of duplicated (update-in-place); the event still fires
    /// so peers/UI can react to the change.
    pub fn receive_remote(&mut self, notif: NotificationData) {
        if let Some(existing) = self
            .store
            .notifications
            .iter_mut()
            .find(|n| identity_key(n) == identity_key(&notif))
        {
            let read = existing.read;
            let replied = existing.replied;
            *existing = notif.clone();
            existing.read = read;
            existing.replied = replied;
        } else {
            self.store.notifications.insert(0, notif.clone());
        }

        if self.store.notifications.len() > NOTIFICATION_HISTORY_MAX {
            self.store
                .notifications
                .truncate(NOTIFICATION_HISTORY_MAX);
        }

        self.store.save(&self.storage_path);
        // NOTE: no event is emitted here. Callers are already inside an
        // EventBus::emit (the HTTP sync handler), and re-emitting the same
        // event would recursively re-enter the subscriber that called us.
    }

    /// Feed a local notification captured outside the poll loop (Android
    /// NotificationListenerService). Legacy shim over [`Self::push_local_rich`]:
    /// matches by content within the last 60s and updates in place.
    pub fn push_local(&mut self, app_name: &str, title: &str, body: &str) -> Option<NotificationData> {
        let rich = RichNotification {
            key: String::new(),
            app_name: app_name.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            ..Default::default()
        };
        self.push_local_rich(rich).map(|r| r.notification)
    }

    /// Feed a rich local notification (Android listener) into the core:
    /// keyed dedup (update in place when the key is already known), history,
    /// event. Returns `None` only for exact-content duplicates within 60s
    /// when no key was provided.
    pub fn push_local_rich(&mut self, rich: RichNotification) -> Option<PushResult> {
        let now = chrono::Utc::now().timestamp_millis();
        let key = if rich.key.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            rich.key.clone()
        };

        // Keyed match first (v2); content match as fallback (v1 shim).
        let existing = self.store.notifications.iter().position(|n| {
            if !rich.key.is_empty() {
                return n.device_id == self.local_device_id && n.notification_key == key;
            }
            n.device_id == self.local_device_id
                && n.app_name == rich.app_name
                && n.title == rich.title
                && n.body == rich.body
                && n.timestamp > now - 60_000
        });

        let is_update = existing.is_some();

        let mut notif = NotificationData {
            id: existing
                .map(|i| self.store.notifications[i].id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            notification_key: key,
            app_package: rich.app_package,
            app_name: rich.app_name,
            title: rich.title,
            body: rich.body,
            icon: rich.icon,
            icon_hash: rich.icon_hash,
            actions: rich.actions,
            can_reply: rich.can_reply,
            messages: rich.messages,
            device_name: self.local_device_name.clone(),
            device_id: self.local_device_id.clone(),
            timestamp: now,
            read: false,
            replied: false,
        };

        match existing {
            Some(i) => {
                notif.read = self.store.notifications[i].read;
                self.store.notifications[i] = notif.clone();
            }
            None => {
                self.store.notifications.insert(0, notif.clone());
            }
        }

        if self.store.notifications.len() > NOTIFICATION_HISTORY_MAX {
            self.store
                .notifications
                .truncate(NOTIFICATION_HISTORY_MAX);
        }

        self.store.save(&self.storage_path);
        self.event_bus
            .emit(EcoEvent::NotificationReceived(notif.clone(), self.local_device_id.clone()));
        // Update cache directly since the event handler uses try_lock()
        crate::eco::notification::set_notifications(self.store.notifications.clone());
        Some(PushResult {
            notification: notif,
            is_update,
        })
    }

    /// Icon dedup: returns `true` (and remembers the hash) only
    /// when the icon for `(peer, key)` hasn't been sent with this hash yet.
    /// The caller should omit the icon bytes on `false` but keep `icon_hash`.
    pub fn should_send_icon(&mut self, peer_id: &str, key: &str, icon_hash: &str) -> bool {
        if icon_hash.is_empty() {
            return false;
        }
        let cache_key = format!("{}::{}", peer_id, key);
        if self.sent_icon_hashes.get(&cache_key).map(|s| s.as_str()) == Some(icon_hash) {
            return false;
        }
        self.sent_icon_hashes.insert(cache_key, icon_hash.to_string());
        true
    }

    /// Remove a locally-originated notification (swiped away on the phone).
    /// The caller fans the dismissal out to peers.
    pub fn remove_local(&mut self, key: &str) -> bool {
        let local_id = self.local_device_id.clone();
        self.remove_remote(&local_id, key)
    }

    /// Remove a notification identified by `(device_id, key)`. Idempotent.
    pub fn remove_remote(&mut self, device_id: &str, key: &str) -> bool {
        let before = self.store.notifications.len();
        self.store.notifications.retain(|n| {
            !(n.device_id == device_id && n.notification_key == key)
        });
        if self.store.notifications.len() != before {
            self.store.save(&self.storage_path);
            return true;
        }
        false
    }

    /// Reply to a notification locally through the source app.
    pub fn reply_to_notification(&self, notification_id: &str, reply_text: &str) -> EcoResult<()> {
        self.platform.reply_to_notification(notification_id, reply_text)
    }

    /// Get all notifications (from UI).
    pub fn get_notifications(&self) -> Vec<NotificationData> {
        self.store.notifications.clone()
    }

    /// Mark a notification as replied (by id or notification key).
    /// Desktop-side: called after a wire reply reaches the originating phone.
    /// Phone-side: not used (its platform reply confirms inline).
    pub fn mark_replied(&mut self, notification_id: &str) {
        if let Some(n) = self
            .store
            .notifications
            .iter_mut()
            .find(|n| n.id == notification_id || n.notification_key == notification_id)
        {
            n.replied = true;
        }
        self.store.save(&self.storage_path);
    }

    /// Get unread count.
    pub fn unread_count(&self) -> usize {
        self.store.notifications.iter().filter(|n| !n.read).count()
    }

    /// Mark a notification as read (by id or notification key).
    pub fn mark_read(&mut self, notification_id: &str) {
        if let Some(n) = self
            .store
            .notifications
            .iter_mut()
            .find(|n| n.id == notification_id || n.notification_key == notification_id)
        {
            n.read = true;
        }
        self.store.save(&self.storage_path);
    }

    /// Clear all notifications.
    pub fn clear_all(&mut self) {
        self.store.notifications.clear();
        self.store.save(&self.storage_path);
    }

    /// Check if platform notification access is available.
    pub fn has_permission(&self) -> bool {
        self.platform.has_permission()
    }

    /// Request platform notification access.
    pub fn request_permission(&self) -> EcoResult<()> {
        self.platform.request_permission()
    }

    /// Set local device identity (called after initialization).
    pub fn set_device_info(&mut self, id: String, name: String) {
        self.local_device_id = id;
        self.local_device_name = name;
    }
}

/// Stable identity of a stored notification: `(device_id, notification_key)`,
/// falling back to the generated `id` for legacy entries without a key.
fn identity_key(n: &NotificationData) -> (String, String) {
    let key = if n.notification_key.is_empty() {
        n.id.clone()
    } else {
        n.notification_key.clone()
    };
    (n.device_id.clone(), key)
}

lazy_static::lazy_static! {
    pub static ref NOTIFICATION_HISTORY: std::sync::Mutex<Option<Vec<NotificationData>>> =
        std::sync::Mutex::new(None);
}

/// Get notifications from the global store.
pub fn get_notifications() -> Vec<NotificationData> {
    NOTIFICATION_HISTORY
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_default()
}

/// Set notifications in the global store (for UI access).
pub fn set_notifications(notifs: Vec<NotificationData>) {
    if let Ok(mut guard) = NOTIFICATION_HISTORY.lock() {
        *guard = Some(notifs);
    }
}
