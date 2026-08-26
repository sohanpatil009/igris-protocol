use crate::eco::errors::EcoResult;

/// Raw notification from the platform notification center.
#[derive(Clone, Debug)]
pub struct RawNotification {
    pub id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub timestamp: i64,
}

/// Platform-specific notification access, injected by the host.
///
/// On Android, `read_notifications` returns empty: notifications arrive
/// event-driven from `NotificationListenerService` and are pushed into the
/// node (inverting the desktop's poll model), so there is nothing to poll here.
/// `has_permission` / `request_permission` still gate the listener.
pub trait PlatformNotification: Send + Sync {
    /// Read current notifications from the system notification center.
    fn read_notifications(&self) -> EcoResult<Vec<RawNotification>>;

    /// Reply to a notification through its source app.
    fn reply_to_notification(&self, notification_id: &str, reply: &str) -> EcoResult<()>;

    /// Check if we have permission to read notifications.
    fn has_permission(&self) -> bool;

    /// Request permission from the user (platform-specific dialog).
    fn request_permission(&self) -> EcoResult<()>;
}
