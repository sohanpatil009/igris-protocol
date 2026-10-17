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
    ///
    /// Default is empty so minimal hosts (the desktop reads nothing from the
    /// system notification center — notifications only arrive over the wire)
    /// keep working without implementing the poll side.
    fn read_notifications(&self) -> EcoResult<Vec<RawNotification>> {
        Ok(Vec::new())
    }

    /// Reply to a notification through its source app.
    ///
    /// Default errors: only hosts whose notification source can execute
    /// replies locally (the phone's `NotificationListenerService` RemoteInput)
    /// implement this; the desktop routes replies over the wire instead.
    fn reply_to_notification(&self, _notification_id: &str, _reply: &str) -> EcoResult<()> {
        Err(crate::eco::errors::EcoError::Notification(
            "reply not available on this platform".to_string(),
        ))
    }

    /// Check if we have permission to read notifications.
    fn has_permission(&self) -> bool;

    /// Request permission from the user (platform-specific dialog).
    fn request_permission(&self) -> EcoResult<()>;

    /// Enterprise both-ways: display a remote notification locally.
    /// Default no-op so existing hosts keep working.
    fn show_notification(&self, _title: &str, _body: &str, _app: &str) -> EcoResult<()> {
        Ok(())
    }
}

/// Enterprise telephony seam (Igris native). Default impls are no-ops.
pub trait PlatformCall: Send + Sync {
    fn mute_call(&self, _call_id: &str) -> EcoResult<()> {
        Ok(())
    }
    fn end_call(&self, _call_id: &str) -> EcoResult<()> {
        Ok(())
    }
}

/// Enterprise SMS seam (Igris native). Default impls are no-ops.
pub trait PlatformSms: Send + Sync {
    fn send_sms(&self, _address: &str, _body: &str) -> EcoResult<()> {
        Ok(())
    }
}
