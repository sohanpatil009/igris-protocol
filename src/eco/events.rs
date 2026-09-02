use crate::eco::clipboard::ClipboardData;
use crate::eco::device::EcoDevice;
use crate::eco::notification::{NotificationData, NotificationReply};
use crate::eco::protocol::{NotificationActionPayload, NotificationDismissPayload};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum EcoEvent {
    DeviceDiscovered(Arc<EcoDevice>),
    DeviceConnected(Arc<EcoDevice>),
    DeviceDisconnected(Arc<EcoDevice>),
    DeviceTrusted(Arc<EcoDevice>),
    DeviceUntrusted(Arc<EcoDevice>),

    ClipboardChanged(Arc<ClipboardData>),
    ClipboardReceived(Arc<ClipboardData>, String),
    ClipboardApplied(Arc<ClipboardData>),

    NotificationReceived(NotificationData, String),
    NotificationReplied(NotificationReply),
    /// A peer dismissed one of our notifications (or a local dismiss event).
    NotificationDismissed(NotificationDismissPayload),
    /// A peer wants us to fire one of the notification's action buttons.
    NotificationActionRequested(NotificationActionPayload),
    /// A peer requests all active notifications (initial sync on connect).
    NotificationRequested,

    PairingRequest(Arc<EcoDevice>),
    PairingAccepted(Arc<EcoDevice>),
    PairingRejected(Arc<EcoDevice>),

    Error(String),
    Synced(String),
}

pub type EventHandler = Arc<dyn Fn(EcoEvent) + Send + Sync>;

pub struct EventBus {
    handlers: std::sync::Mutex<Vec<EventHandler>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            handlers: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn subscribe(&self, handler: EventHandler) {
        self.handlers.lock().unwrap().push(handler);
    }

    pub fn emit(&self, event: EcoEvent) {
        // Clone under lock, then invoke outside it: handlers may emit
        // nested events (e.g. receive_remote re-emitting NotificationReceived),
        // which would otherwise deadlock on the non-reentrant mutex.
        let handlers = self.handlers.lock().unwrap().clone();
        for handler in handlers.iter() {
            handler(event.clone());
        }
    }
}
