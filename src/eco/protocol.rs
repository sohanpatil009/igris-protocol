use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const CURRENT_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EcoMessage {
    pub version: String,
    pub msg_type: MessageType,
    pub sender_id: String,
    pub sender_name: String,
    pub payload: serde_json::Value,
    pub timestamp: i64,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum MessageType {
    Announcement(DeviceAnnouncement),
    ClipboardSync(ClipboardSyncPayload),
    ClipboardPullRequest(ClipboardPullRequest),
    ClipboardPullResponse(ClipboardPullResponse),
    NotificationSync(NotificationSyncPayload),
    NotificationReply(NotificationReplyPayload),
    NotificationDismissed(NotificationDismissPayload),
    NotificationAction(NotificationActionPayload),
    CallSync(CallSyncPayload),
    CallAction(CallActionPayload),
    CallLogSync(CallLogSyncPayload),
    SmsSync(SmsSyncPayload),
    PairingRequest(PairingPayload),
    PairingResponse(PairingPayload),
    Heartbeat,
    Ack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceAnnouncement {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub hostname: String,
    pub version: String,
    pub capabilities: HashMap<String, bool>,
    pub public_key: Option<String>,
    pub eco_port: u16,
    pub ip_address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardSyncPayload {
    pub content_hash: String,
    pub content: String,
    pub content_type: String,
    pub source_device: String,
    pub timestamp: i64,
    /// Enterprise media extension (backward compatible):
    /// `encoding` is "text" (default) or "base64" for image/file bytes.
    #[serde(default)]
    pub encoding: String,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub byte_size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardPullRequest {
    pub content_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardPullResponse {
    pub content_hash: String,
    pub content: String,
    pub content_type: String,
    pub success: bool,
}

/// A single message in a MessagingStyle conversation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationMessage {
    pub sender: String,
    pub text: String,
}

/// Notification synced from a remote device.
///
/// Fields added in protocol v2 are all `#[serde(default)]` so peers running
/// the old wire format keep interoperating: a v1 sender omits them and the
/// receiver sees empty defaults; a v2 sender adds them and v1 receivers
/// ignore unknown JSON fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationSyncPayload {
    pub notification_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub source_device_id: String,
    pub source_device_name: String,
    pub timestamp: i64,
    /// Stable identity from the Android `StatusBarNotification.key`.
    /// Empty for legacy senders (falls back to `notification_id`).
    #[serde(default)]
    pub notification_key: String,
    #[serde(default)]
    pub app_package: String,
    /// Base64 PNG (<=96px, downscaled on the phone). `None` when the peer
    /// already has this icon (`icon_hash` unchanged — hash-skip).
    #[serde(default)]
    pub icon: Option<String>,
    /// MD5 hex of the icon bytes; the receiver caches icons by this hash.
    #[serde(default)]
    pub icon_hash: String,
    /// Plain action button labels (reply actions excluded).
    #[serde(default)]
    pub actions: Vec<String>,
    /// True when the notification carries a `RemoteInput` quick-reply.
    #[serde(default)]
    pub can_reply: bool,
    /// MessagingStyle conversation entries.
    #[serde(default)]
    pub messages: Vec<NotificationMessage>,
    /// True when this is an update of an already-synced key (peers should
    /// update in place instead of popping a new toast).
    #[serde(default)]
    pub is_update: bool,
}

/// Reply to a notification sent back to the originating device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationReplyPayload {
    pub notification_id: String,
    pub reply_text: String,
    pub source_device_id: String,
    /// The originating `notification_key` (v2). Legacy peers omit it.
    #[serde(default)]
    pub notification_key: String,
}

/// Dismiss a notification on the originating device (swiped away / cancelled).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationDismissPayload {
    pub notification_key: String,
    pub source_device_id: String,
}

/// Trigger an action button on the originating device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationActionPayload {
    pub notification_key: String,
    pub action_index: u32,
    pub source_device_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairingPayload {
    pub device_id: String,
    pub device_name: String,
    pub public_key: Option<String>,
    pub accepted: bool,
    pub message: Option<String>,
}

/// Enterprise call event (Igris native, no external telephony ref).
/// `event`: ringing | missed | talking | ended
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallSyncPayload {
    pub call_id: String,
    pub event: String,
    #[serde(default)]
    pub phone_number: String,
    #[serde(default)]
    pub contact_name: String,
    #[serde(default)]
    pub contact_thumbnail_hash: String,
    pub source_device_id: String,
    pub source_device_name: String,
    pub timestamp: i64,
    #[serde(default)]
    pub is_cancel: bool,
}

/// Enterprise call control: mute | accept | decline | end | dial.
/// `dial` carries its target in `phone_number` (desk-initiated outgoing call).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallActionPayload {
    pub call_id: String,
    pub action: String,
    pub source_device_id: String,
    #[serde(default)]
    pub phone_number: String,
    #[serde(default)]
    pub contact_name: String,
}

/// Enterprise phonebook search: desktop queries a linked phone by name or
/// number fragment; the phone answers from its ContentProvider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactsSearchRequest {
    pub requester_id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_contacts_limit")]
    pub limit: u32,
}

fn default_contacts_limit() -> u32 {
    10
}

/// Enterprise SMS send request: desktop composes, phone's SIM transmits.
/// Trust-gated; the phone executes via its SMS seam, never the desktop.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SmsSendPayload {
    pub address: String,
    pub body: String,
    pub source_device_id: String,
}

/// Enterprise call-log snapshot: a batch of call-history rows exchanged
/// between linked peers so the desktop mirrors the phone's call log.
/// `call_type`: incoming | outgoing | missed | voicemail | rejected | blocked | answered_externally | unknown
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallLogSyncPayload {
    pub calls: Vec<CallLogEntry>,
    pub source_device_id: String,
    pub source_device_name: String,
    pub timestamp: i64,
}

/// One call-history row carried on the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallLogEntry {
    pub call_id: String,
    pub event: String,
    pub phone_number: String,
    pub contact_name: String,
    pub device_name: String,
    pub device_id: String,
    pub timestamp: i64,
    pub duration_seconds: i64,
    pub call_type: String,
    pub is_cancel: bool,
}

/// Enterprise SMS (v1): single message envelope, batched by sender.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SmsSyncPayload {
    pub sms_id: String,
    pub thread_id: String,
    pub address: String,
    pub body: String,
    pub date: i64,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub sub_id: i64,
    pub source_device_id: String,
    pub source_device_name: String,
    pub timestamp: i64,
}

impl EcoMessage {
    pub fn new(msg_type: MessageType, sender_id: String, sender_name: String) -> Self {
        let payload = serde_json::to_value(&msg_type).unwrap_or_default();
        Self {
            version: CURRENT_VERSION.to_string(),
            msg_type,
            sender_id,
            sender_name,
            payload,
            timestamp: chrono::Utc::now().timestamp_millis(),
            signature: None,
        }
    }
}
