//! UniFFI surface wrapping the shared eco + fastswap core for the mobile
//! companions. The Kotlin side implements the platform seams (`ClipboardProvider`,
//! `NotificationProvider`) and observes events through `NodeEventListener`.
//!
//! Everything in this module is `pub` and `#[uniffi::export]`-annotated so the
//! bindgen emits Kotlin (library mode). The node owns a tokio runtime and the
//! `EcoManager`; foreign calls block on that runtime internally.

use crate::eco::device::EcoDevice;
use crate::eco::events::{EcoEvent, EventBus};
use crate::eco::manager::EcoManager;
use crate::eco::notification::NotificationData;
use crate::platform::ecosystem::notifications::{PlatformNotification, RawNotification};
use crate::platform::ecosystem::PlatformClipboard;
use std::path::PathBuf;
use std::sync::Arc;

/// Android-only logging: route `log` + `tracing` records to logcat under the
/// "IGRIS" tag so foreground diagnostics are visible via `adb logcat -s IGRIS`.
/// Idempotent across node restarts (the log facade can only be initialized
/// once per process).
#[cfg(target_os = "android")]
fn init_android_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("IGRIS"),
        );
        // Forward `tracing` events (fastswap/eco) into `log` → logcat.
        let _ = tracing::subscriber::set_global_default(tracing_log::TraceLogger::new());
    });
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[derive(uniffi::Error)]
pub enum FfiError {
    #[error("{0}")]
    Message(String),
}

pub type FfiResult<T> = Result<T, FfiError>;

fn err<E: std::fmt::Display>(e: E) -> FfiError {
    FfiError::Message(e.to_string())
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Everything the host needs to boot a node. `device_id` must be a lowercase
/// canonical UUID; it is persisted by the host (DataStore) and reused across
/// restarts so pairing/trust survives.
#[derive(uniffi::Record)]
pub struct NodeConfig {
    pub device_id: String,
    pub name: String,
    pub model: String,
    pub data_dir: String,
    pub downloads_dir: String,
    pub serve_plain_http: bool,
    pub clipboard_enabled: bool,
    pub notification_enabled: bool,
}

/// Snapshot of a peer seen on the LAN (eco discovery + pairing).
#[derive(uniffi::Record)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub ip: String,
    pub port: u16,
    pub is_trusted: bool,
    pub is_online: bool,
    pub last_seen_secs: u64,
    /// True when the peer identifies as a mobile device (via `/info`
    /// `device_type`). Drives the mobile-gated keep-alive wake locks.
    pub is_mobile: bool,
}

/// A notification synced from any device.
#[derive(uniffi::Record)]
pub struct IncomingNotification {
    pub id: String,
    /// Stable identity from the Android `StatusBarNotification.key`.
    pub notification_key: String,
    pub app_package: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    /// Base64 PNG (<=96px), `None` when the peer already cached it.
    pub icon: Option<String>,
    pub icon_hash: String,
    pub actions: Vec<String>,
    pub can_reply: bool,
    pub messages: Vec<NotificationMessageInfo>,
    pub device_name: String,
    pub device_id: String,
    pub timestamp: i64,
    pub read: bool,
    pub replied: bool,
}

/// A single MessagingStyle conversation entry.
#[derive(uniffi::Record)]
pub struct NotificationMessageInfo {
    pub sender: String,
    pub text: String,
}

/// Rich notification input captured by the Android NotificationListenerService.
#[derive(uniffi::Record)]
pub struct RichNotificationInfo {
    pub key: String,
    pub app_package: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub icon: Option<String>,
    pub icon_hash: String,
    pub actions: Vec<String>,
    pub can_reply: bool,
    pub messages: Vec<NotificationMessageInfo>,
}

/// A pending inbound FastSwap transfer awaiting the user's accept/deny.
#[derive(uniffi::Record)]
pub struct IncomingTransfer {
    pub session_id: String,
    pub sender_name: String,
    pub sender_device: String,
    pub file_count: u64,
    pub total_size: u64,
    pub files: Vec<String>,
}

/// Live progress of an in/out transfer.
#[derive(uniffi::Record)]
pub struct TransferProgressInfo {
    pub session_id: String,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub overall_percent: f64,
    pub is_complete: bool,
    /// True for sessions we initiated (send), false for inbound receives.
    pub is_outbound: bool,
    /// File names in this session, in transfer order.
    pub file_names: Vec<String>,
}

/// A file the user picked to send (Phase 3 wiring).
#[derive(uniffi::Record)]
pub struct OutgoingFile {
    pub path: String,
    pub relative_path: String,
}

/// Clipboard payload moved across devices.
#[derive(uniffi::Record)]
pub struct ClipboardDataInfo {
    pub content: String,
    pub content_type: String,
    pub content_hash: String,
    pub source_device: String,
    pub timestamp: i64,
}

/// Raw notification handed back from the platform (used by the
/// `NotificationProvider` stub/real implementation).
#[derive(uniffi::Record)]
pub struct RawNotificationInfo {
    pub id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// Callback interfaces (implemented in Kotlin)
// ---------------------------------------------------------------------------

/// Platform clipboard, injected by the host. Android 10+ restricts background
/// reads; `get_text` may return empty while the app is backgrounded.
#[uniffi::export(foreign)]
pub trait ClipboardProvider: Send + Sync {
    fn get_text(&self) -> FfiResult<String>;
    fn set_text(&self, text: String) -> FfiResult<()>;
}

/// Platform notification access, injected by the host. On Android,
/// `read_notifications` returns empty (events arrive from the listener service);
/// the permission gates still drive the listener onboarding.
#[uniffi::export(foreign)]
pub trait NotificationProvider: Send + Sync {
    fn read_notifications(&self) -> FfiResult<Vec<RawNotificationInfo>>;
    fn reply_to_notification(&self, notification_id: String, reply: String) -> FfiResult<()>;
    fn has_permission(&self) -> bool;
    fn request_permission(&self) -> FfiResult<()>;
}

/// Events pushed up from the core to the UI.
#[uniffi::export(foreign)]
pub trait NodeEventListener: Send + Sync {
    fn on_device_updated(&self, device: DeviceInfo);
    fn on_device_removed(&self, device_id: String);
    fn on_clipboard_received(&self, data: ClipboardDataInfo);
    /// Remote-only variant: fires when a peer's clipboard arrived on this
    /// device (not for locally-detected changes).
    fn on_clipboard_received_from(&self, data: ClipboardDataInfo);
    fn on_notification_received(&self, notification: IncomingNotification);
    /// A peer wants us to fire the quick-reply `RemoteInput` of the
    /// notification identified by `key` with the given text.
    fn on_notification_reply_requested(&self, key: String, text: String);
    /// A peer wants us to fire the `action_index`-th action button of the
    /// notification identified by `key`.
    fn on_notification_action_requested(&self, key: String, action_index: u32);
    /// A peer dismissed our notification `key` — cancel it locally.
    fn on_notification_dismiss_requested(&self, key: String);
    fn on_transfer_request(&self, transfer: IncomingTransfer);
    fn on_transfer_progress(&self, progress: TransferProgressInfo);
    fn on_log(&self, message: String);
}

// ---------------------------------------------------------------------------
// Adapters: FFI callback traits -> core platform traits
// ---------------------------------------------------------------------------

struct ClipboardProviderAdapter {
    inner: Arc<dyn ClipboardProvider>,
}

impl PlatformClipboard for ClipboardProviderAdapter {
    fn get_text(&self) -> crate::eco::errors::EcoResult<String> {
        self.inner.get_text().map_err(|e| crate::eco::errors::EcoError::Clipboard(e.to_string()))
    }

    fn set_text(&self, text: &str) -> crate::eco::errors::EcoResult<()> {
        self.inner
            .set_text(text.to_string())
            .map_err(|e| crate::eco::errors::EcoError::Clipboard(e.to_string()))
    }
}

struct NotificationProviderAdapter {
    inner: Arc<dyn NotificationProvider>,
}

impl PlatformNotification for NotificationProviderAdapter {
    fn read_notifications(&self) -> crate::eco::errors::EcoResult<Vec<RawNotification>> {
        let raw = self
            .inner
            .read_notifications()
            .map_err(|e| crate::eco::errors::EcoError::Notification(e.to_string()))?;
        Ok(raw
            .into_iter()
            .map(|r| RawNotification {
                id: r.id,
                app_name: r.app_name,
                title: r.title,
                body: r.body,
                timestamp: r.timestamp,
            })
            .collect())
    }

    fn reply_to_notification(&self, notification_id: &str, reply: &str) -> crate::eco::errors::EcoResult<()> {
        self.inner
            .reply_to_notification(notification_id.to_string(), reply.to_string())
            .map_err(|e| crate::eco::errors::EcoError::Notification(e.to_string()))
    }

    fn has_permission(&self) -> bool {
        self.inner.has_permission()
    }

    fn request_permission(&self) -> crate::eco::errors::EcoResult<()> {
        self.inner
            .request_permission()
            .map_err(|e| crate::eco::errors::EcoError::Notification(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Record conversions
// ---------------------------------------------------------------------------

fn device_info_of(device: &EcoDevice) -> DeviceInfo {
    DeviceInfo {
        id: device.id.to_string(),
        name: device.name.clone(),
        hostname: device.hostname.clone(),
        ip: device
            .addr
            .map(|a| a.ip().to_string())
            .unwrap_or_default(),
        port: device.addr.map(|a| a.port()).unwrap_or(crate::eco::constants::ECO_TLS_PORT),
        is_trusted: device.is_trusted(),
        is_online: device.is_online(),
        last_seen_secs: 0,
        is_mobile: device.platform.eq_ignore_ascii_case("mobile"),
    }
}

fn discovered_info_of(device: &crate::eco::discovery::DiscoveredEcoDevice) -> DeviceInfo {
    DeviceInfo {
        id: device.id.clone(),
        name: device.name.clone(),
        hostname: device.hostname.clone(),
        ip: device.ip.clone(),
        port: device.port,
        is_trusted: device.is_trusted,
        is_online: device.is_online,
        last_seen_secs: device.last_seen_secs,
        is_mobile: device.is_mobile,
    }
}

/// Build a full `EcoDevice` from a discovery entry so pairing events carry a
/// usable snapshot for `on_device_updated`.
fn eco_device_of(device: &crate::eco::discovery::DiscoveredEcoDevice) -> EcoDevice {
    let uuid = uuid::Uuid::parse_str(&device.id).unwrap_or_else(|_| uuid::Uuid::new_v4());
    let mut dev = EcoDevice::new(device.name.clone());
    dev.id = uuid;
    dev.status = if device.is_trusted {
        crate::eco::device::DeviceStatus::Trusted
    } else {
        crate::eco::device::DeviceStatus::Online
    };
    dev.addr = format!("{}:{}", device.ip, device.port).parse().ok();
    dev
}

fn notification_info_of(n: &NotificationData) -> IncomingNotification {
    IncomingNotification {
        id: n.id.clone(),
        notification_key: n.notification_key.clone(),
        app_package: n.app_package.clone(),
        app_name: n.app_name.clone(),
        title: n.title.clone(),
        body: n.body.clone(),
        icon: n.icon.clone(),
        icon_hash: n.icon_hash.clone(),
        actions: n.actions.clone(),
        can_reply: n.can_reply,
        messages: n
            .messages
            .iter()
            .map(|m| NotificationMessageInfo {
                sender: m.sender.clone(),
                text: m.text.clone(),
            })
            .collect(),
        device_name: n.device_name.clone(),
        device_id: n.device_id.clone(),
        timestamp: n.timestamp,
        read: n.read,
        replied: n.replied,
    }
}

fn clipboard_info_of(data: &crate::eco::clipboard::ClipboardData) -> ClipboardDataInfo {
    ClipboardDataInfo {
        content: data.content.clone(),
        content_type: data.content_type.clone(),
        content_hash: data.content_hash.clone(),
        source_device: data.source_device.clone(),
        timestamp: data.timestamp,
    }
}

// ---------------------------------------------------------------------------
// The node object
// ---------------------------------------------------------------------------

/// Owns the tokio runtime + managers for one app process. Construct once,
/// keep for the life of the foreground service.
#[derive(uniffi::Object)]
pub struct EcosystemNode {
    runtime: tokio::runtime::Runtime,
    manager: std::sync::Mutex<EcoManager>,
    event_bus: Arc<EventBus>,
    listener: Option<Arc<dyn NodeEventListener>>,
}

#[uniffi::export]
impl EcosystemNode {    /// Build the node: apply config, override identity, initialize the eco
    /// manager and FastSwap storage seams. Does not bind any ports — call
    /// [`EcosystemNode::start`] to boot servers + discovery.
    #[uniffi::constructor]
    pub fn new(
        config: NodeConfig,
        clipboard_provider: Arc<dyn ClipboardProvider>,
        notification_provider: Arc<dyn NotificationProvider>,
        listener: Option<Arc<dyn NodeEventListener>>,
    ) -> FfiResult<Arc<Self>> {
        #[cfg(target_os = "android")]
        init_android_logging();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(err)?;

        let pkg_dir = PathBuf::from(&config.data_dir);
        std::fs::create_dir_all(&pkg_dir).map_err(err)?;
        std::fs::create_dir_all(&config.downloads_dir).map_err(err)?;

        let mut manager = EcoManager::new(&pkg_dir);
        {
            let cfg = manager.config_mut();
            cfg.enabled = true;
            cfg.clipboard_sync = config.clipboard_enabled;
            cfg.notification_sync = config.notification_enabled;
            cfg.device_name = config.name.clone();
        }

        // Override identity (device id/name/model) BEFORE initialize so the
        // notification manager captures the canonical values.
        {
            let uuid = uuid::Uuid::parse_str(&config.device_id)
                .map_err(|_| FfiError::Message(format!("invalid device_id {}", config.device_id)))?;
            let local_device = manager.local_device().clone();
            runtime.block_on(async move {
                let mut dev = local_device.write().await;
                dev.id = uuid;
                dev.name = config.name.clone();
                dev.hostname = config.model.clone();
            });
        }

        // Storage seams for FastSwap (certs + downloads live under app data on
        // Android; the desktop fallbacks are inert until the converge step).
        let certs_dir = pkg_dir.join("certs");
        crate::fastswap::set_fastswap_paths(certs_dir, PathBuf::from(&config.downloads_dir));

        let clipboard = ClipboardProviderAdapter {
            inner: clipboard_provider,
        };
        let notification = NotificationProviderAdapter {
            inner: notification_provider,
        };

        let event_bus: Arc<EventBus>;
        {
            runtime
                .block_on(manager.initialize(&pkg_dir, Box::new(clipboard), Box::new(notification)))
                .map_err(err)?;
            event_bus = manager.event_bus().clone();
        }

        if let Some(ref listener) = listener {
            let l = listener.clone();
            event_bus.subscribe(Arc::new(move |event| bridge_event(&l, event)));
        }

        Ok(Arc::new(Self {
            runtime,
            manager: std::sync::Mutex::new(manager),
            event_bus,
            listener,
        }))
    }

    /// Bind the eco HTTP/TLS ports, start discovery, the notification/clipboard
    /// sync loops, and the FastSwap (LocalSend v2) server so the phone is
    /// discoverable on both protocols.
    pub fn start(&self) -> FfiResult<()> {
        {
            let mut manager = self.manager.lock().map_err(err)?;
            self.runtime.block_on(manager.start()).map_err(err)?;
        }

        let (id, name, model) = {
            let manager = self.manager.lock().map_err(err)?;
            self.runtime.block_on(async {
                let dev = manager.local_device().read().await;
                (dev.id.to_string(), dev.name.clone(), dev.hostname.clone())
            })
        };

        // FastSwap device identity (Mobile, real id/alias/model) injected into
        // the on-demand global before the server boots.
        let local_ip = local_ip_address::local_ip()
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 100)))
            .to_string();
        let device = crate::fastswap::models::Device {
            id,
            alias: name,
            device_model: model,
            device_type: crate::fastswap::models::DeviceType::Mobile,
            ip: local_ip,
            port: 53317,
            protocol: "2.0".to_string(),
            download: true,
        };
        if let Ok(mut guard) = crate::fastswap::FASTSWAP_DEVICE.lock() {
            *guard = Some(device);
        }

        // Alert hook: surface inbound transfers to the host immediately (the
        // 60 s approval window is too short to rely on UI polling alone).
        if let Some(listener) = self.listener.clone() {
            crate::fastswap::on_pending_transfer(Arc::new(move |t| {
                listener.on_transfer_request(IncomingTransfer {
                    session_id: t.session_id.clone(),
                    sender_name: t.sender_name.clone(),
                    sender_device: t.sender_device.clone(),
                    file_count: t.file_count as u64,
                    total_size: t.total_size,
                    files: t.files.clone(),
                });
            }));
        }

        self.runtime.block_on(crate::fastswap::start_on_demand());

        Ok(())
    }

    pub fn shutdown(&self) {
        if let Ok(mut manager) = self.manager.lock() {
            self.runtime.block_on(manager.shutdown());
        }
    }

    /// Current view of discovered + paired peers (eco subnet scan).
    pub fn get_devices(&self) -> Vec<DeviceInfo> {
        self.runtime.block_on(async {
            let devices = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
            devices.iter().map(discovered_info_of).collect()
        })
    }

    /// Number of devices this node has ever trusted (persisted, so it survives
    /// reboots and is non-zero at boot before any live discovery). Drives the
    /// "stay alive while linked" keep-alive tier without waiting for a scan.
    pub fn trusted_count(&self) -> u32 {
        crate::eco::pairing::trusted_count()
    }

    /// Phone-initiated LINK: send a pairing request to the peer at `host` (IP)
    /// and trust it locally. The peer's `/pair/request` handler trusts us too,
    /// so this is a direct two-way link.
    pub fn pair(&self, host: String) -> FfiResult<()> {
        self.runtime.block_on(async {
            let peer = {
                let devices = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                devices.iter().find(|d| d.ip == host).cloned()
            };
            let peer = peer.ok_or_else(|| {
                FfiError::Message(format!("no device at {host} — run a scan first"))
            })?;

            let local_id = crate::eco::pairing::get_local_device_id()
                .ok_or_else(|| FfiError::Message("local identity not set".to_string()))?;
            let local_name = crate::eco::pairing::get_local_device_name()
                .ok_or_else(|| FfiError::Message("local identity not set".to_string()))?;

            // Persist trust BEFORE sending the request so that our /info
            // includes this device in trusted_peers immediately — prevents
            // the peer's stale-trust detection from unlinking us.
            if let Some(manager) = crate::eco::pairing::get_pairing_manager() {
                if let Some(mgr) = manager.as_ref() {
                    mgr.trust_device_direct(&peer.id).await.map_err(err)?;
                }
            }
            {
                let mut devices = crate::eco::discovery::ECO_NETWORK_DEVICES.write().await;
                if let Some(d) = devices.iter_mut().find(|d| d.id == peer.id) {
                    d.is_trusted = true;
                }
            }

            // Use the peer's actual discovered address (port-aware) instead
            // of hardcoding ECO_TLS_PORT which fails for HTTP-only peers.
            let addr = peer
                .ip
                .parse::<std::net::IpAddr>()
                .map_err(err)
                .map(|ip| {
                    let port = peer.port;
                    std::net::SocketAddr::new(ip, port)
                })?;
            let transport = crate::eco::transport::EcoTransport::new();
            transport
                .send_pair_request(
                    &addr,
                    &local_id,
                    &local_name,
                    crate::eco::constants::DEFAULT_ECO_PORT,
                )
                .await
                .map_err(err)?;

            self.event_bus
                .emit(crate::eco::events::EcoEvent::DeviceTrusted(Arc::new(
                    eco_device_of(&peer),
                )));
            Ok(())
        })
    }

    /// Phone-initiated UNLINK: tell the peer (by its device id) to stop
    /// trusting us and untrust it locally.
    pub fn unpair(&self, device_id: String) -> FfiResult<()> {
        self.runtime.block_on(async {
            let peer = {
                let devices = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                devices.iter().find(|d| d.id == device_id).cloned()
            };

            let local_id = crate::eco::pairing::get_local_device_id()
                .ok_or_else(|| FfiError::Message("local identity not set".to_string()))?;

            if let Some(peer) = &peer {
                let addr = peer
                    .ip
                    .parse::<std::net::IpAddr>()
                    .map_err(err)
                    .map(|ip| {
                        std::net::SocketAddr::new(ip, crate::eco::constants::ECO_TLS_PORT)
                    })?;
                let transport = crate::eco::transport::EcoTransport::new();
                transport.send_untrust(&addr, &local_id).await.map_err(err)?;
            }

            if let Some(manager) = crate::eco::pairing::get_pairing_manager() {
                if let Some(mgr) = manager.as_ref() {
                    mgr.remove_trust(&device_id).await.map_err(err)?;
                }
            }
            {
                let mut devices = crate::eco::discovery::ECO_NETWORK_DEVICES.write().await;
                if let Some(d) = devices.iter_mut().find(|d| d.id == device_id) {
                    d.is_trusted = false;
                }
            }
            self.event_bus
                .emit(crate::eco::events::EcoEvent::DeviceUntrusted(Arc::new(
                    EcoDevice::new(device_id),
                )));
            Ok(())
        })
    }

    /// Push a clipboard change detected outside the poll loop (accessibility
    /// hook / in-app paste). Feeds the same dedup + trusted-peers fan-out as
    /// a locally-detected change. A failed fan-out is NOT committed, so an
    /// identical re-push is retried once a peer is reachable.
    pub fn push_clipboard(&self, text: String) -> FfiResult<()> {
        self.runtime.block_on(async {
            let manager = self.manager.lock().map_err(err)?;
            let clipboard = manager
                .clipboard()
                .ok_or_else(|| FfiError::Message("clipboard manager not ready".to_string()))?;
            let mut guard = clipboard
                .lock()
                .map_err(|_| FfiError::Message("clipboard lock failed".to_string()))?;
            guard
                .push_local_change(&text)
                .map_err(|e| FfiError::Message(e.to_string()))
        })
    }

    /// True when the most recent local clipboard content was delivered to at
    /// least one linked peer. The a11y hook polls this to retry a push that
    /// failed while peers were offline (dedup-on-failed-send fix).
    pub fn clipboard_delivered(&self) -> bool {
        self.runtime.block_on(async {
            let manager = self.manager.lock().map_err(err)?;
            let clipboard = manager
                .clipboard()
                .ok_or_else(|| FfiError::Message("clipboard manager not ready".to_string()))?;
            let guard = clipboard
                .lock()
                .map_err(|_| FfiError::Message("clipboard lock failed".to_string()))?;
            Ok::<bool, FfiError>(guard.is_current_delivered())
        })
        .unwrap_or(false)
    }

    /// Feed a local notification (from the NotificationListenerService) into
    /// the core: history + event, then fan-out to trusted (linked) peers over
    /// `/notification/sync`. Dedups against the last 60s of history.
    pub fn push_notification(&self, app_name: String, title: String, body: String) -> FfiResult<()> {
        self.push_notification_rich(RichNotificationInfo {
            key: String::new(),
            app_package: String::new(),
            app_name,
            title,
            body,
            icon: None,
            icon_hash: String::new(),
            actions: Vec::new(),
            can_reply: false,
            messages: Vec::new(),
        })
    }

    /// Feed a rich local notification (NotificationListenerService capture)
    /// into the core: keyed dedup (updates in place), history, event, then
    /// fan-out to trusted peers with the `notification_sync` capability over
    /// `/notification/sync`. Icon bytes are skipped per-peer when the hash is
    /// unchanged (KDE-style); `icon_hash` is still sent so the peer can cache.
    pub fn push_notification_rich(&self, rich: RichNotificationInfo) -> FfiResult<()> {
        self.runtime.block_on(async {
            let (notif, is_update) = {
                let manager = self.manager.lock().map_err(err)?;
                let notification = manager
                    .notification_manager()
                    .ok_or_else(|| FfiError::Message("notification manager not ready".to_string()))?;
                let mut guard = notification
                    .lock()
                    .map_err(|_| FfiError::Message("notification lock failed".to_string()))?;
                let rich_inner = crate::eco::notification::RichNotification {
                    key: rich.key.clone(),
                    app_package: rich.app_package.clone(),
                    app_name: rich.app_name.clone(),
                    title: rich.title.clone(),
                    body: rich.body.clone(),
                    icon: rich.icon.clone(),
                    icon_hash: rich.icon_hash.clone(),
                    actions: rich.actions.clone(),
                    can_reply: rich.can_reply,
                    messages: rich
                        .messages
                        .iter()
                        .map(|m| crate::eco::protocol::NotificationMessage {
                            sender: m.sender.clone(),
                            text: m.text.clone(),
                        })
                        .collect(),
                };
                match guard.push_local_rich(rich_inner) {
                    Some(result) => (result.notification, result.is_update),
                    None => return Ok(()),
                }
            };

            // Fan out to trusted + reachable peers only.
            let trusted: std::collections::HashSet<String> = {
                let net = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                net.iter()
                    .filter(|d| d.is_trusted)
                    .map(|d| d.id.clone())
                    .collect()
            };
            let mut targets: Vec<(String, std::net::SocketAddr)> = Vec::new();
            {
                let manager = self.manager.lock().map_err(err)?;
                if let Some(discovery) = manager.discovery() {
                    let known = discovery.get_known_devices();
                    let peers = known.read().await;
                    for (id, d) in peers.iter() {
                        if !d.device.capabilities.notification_sync {
                            continue;
                        }
                        if !trusted.contains(id) {
                            continue;
                        }
                        if let Some(addr) = d.device.addr {
                            targets.push((id.clone(), addr));
                        }
                    }
                }
            }

            let transport = crate::eco::transport::EcoTransport::new();
            for (peer_id, addr) in targets {
                let mut payload = crate::eco::protocol::NotificationSyncPayload {
                    notification_id: notif.id.clone(),
                    notification_key: notif.notification_key.clone(),
                    app_package: notif.app_package.clone(),
                    app_name: notif.app_name.clone(),
                    title: notif.title.clone(),
                    body: notif.body.clone(),
                    icon: notif.icon.clone(),
                    icon_hash: notif.icon_hash.clone(),
                    actions: notif.actions.clone(),
                    can_reply: notif.can_reply,
                    messages: notif.messages.clone(),
                    is_update,
                    source_device_id: notif.device_id.clone(),
                    source_device_name: notif.device_name.clone(),
                    timestamp: notif.timestamp,
                };
                // KDE-style icon dedup: skip the bytes when this peer already
                // saw this hash for this key.
                let send_icon = {
                    let manager = self.manager.lock().map_err(err)?;
                    if let Some(notification) = manager.notification_manager() {
                        if let Ok(mut guard) = notification.lock() {
                            guard.should_send_icon(
                                &peer_id,
                                &payload.notification_key,
                                &payload.icon_hash,
                            )
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if !send_icon {
                    payload.icon = None;
                }
                let _ = transport.send_notification(&addr, &payload).await;
            }
            Ok(())
        })
    }

    /// Mirror a locally-dismissed notification (swiped away on the phone):
    /// remove it from local history and fan the dismissal out to trusted
    /// peers with the `notification_sync` capability over
    /// `/notification/dismiss`.
    pub fn dismiss_notification(&self, key: String) -> FfiResult<()> {
        self.runtime.block_on(async {
            let removed = {
                let manager = self.manager.lock().map_err(err)?;
                let notification = manager
                    .notification_manager()
                    .ok_or_else(|| FfiError::Message("notification manager not ready".to_string()))?;
                let mut guard = notification
                    .lock()
                    .map_err(|_| FfiError::Message("notification lock failed".to_string()))?;
                guard.remove_local(&key)
            };
            if !removed {
                // Unknown locally, but peers may still hold it — forward anyway.
            }

            let local_id = crate::eco::pairing::get_local_device_id()
                .unwrap_or_default();
            let trusted: std::collections::HashSet<String> = {
                let net = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                net.iter()
                    .filter(|d| d.is_trusted)
                    .map(|d| d.id.clone())
                    .collect()
            };
            let mut targets: Vec<std::net::SocketAddr> = Vec::new();
            {
                let manager = self.manager.lock().map_err(err)?;
                if let Some(discovery) = manager.discovery() {
                    let known = discovery.get_known_devices();
                    let peers = known.read().await;
                    for (id, d) in peers.iter() {
                        if !d.device.capabilities.notification_sync {
                            continue;
                        }
                        if !trusted.contains(id) {
                            continue;
                        }
                        if let Some(addr) = d.device.addr {
                            targets.push(addr);
                        }
                    }
                }
            }

            let payload = crate::eco::protocol::NotificationDismissPayload {
                notification_key: key,
                source_device_id: local_id,
            };
            let transport = crate::eco::transport::EcoTransport::new();
            for addr in targets {
                let _ = transport.send_notification_dismiss(&addr, &payload).await;
            }
            Ok(())
        })
    }

    /// This node's canonical device id (the host's persisted UUID). Lets the
    /// UI distinguish locally-originated events from remote ones.
    pub fn local_device_id(&self) -> String {
        crate::eco::pairing::get_local_device_id().unwrap_or_default()
    }

    /// Pending inbound FastSwap transfers waiting on accept/deny.
    pub fn pending_transfers(&self) -> Vec<IncomingTransfer> {
        self.runtime.block_on(async {
            crate::fastswap::get_pending_transfers()
                .await
                .into_iter()
                .map(|t| IncomingTransfer {
                    session_id: t.session_id,
                    sender_name: t.sender_name,
                    sender_device: t.sender_device,
                    file_count: t.file_count as u64,
                    total_size: t.total_size,
                    files: t.files,
                })
                .collect()
        })
    }

    pub fn approve_transfer(&self, session_id: String) {
        self.runtime.block_on(crate::fastswap::approve_transfer(&session_id));
    }

    pub fn deny_transfer(&self, session_id: String) {
        self.runtime.block_on(crate::fastswap::deny_transfer(&session_id));
    }

    /// Send staged files (absolute paths, e.g. `cacheDir/outbox/...`) to the
    /// peer at `host` over FastSwap (LocalSend v2 three-way handshake +
    /// parallel uploads). Returns the local session id for progress tracking.
    /// Long-running — call from a background thread.
    pub fn send_files(&self, host: String, files: Vec<OutgoingFile>) -> FfiResult<String> {
        self.runtime.block_on(async {
            if files.is_empty() {
                return Err(FfiError::Message("no files to send".to_string()));
            }
            let peer = {
                let devices = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                devices.iter().find(|d| d.ip == host).cloned()
            };
            let peer = peer.ok_or_else(|| {
                FfiError::Message(format!("no device at {host} — run a scan first"))
            })?;

            let target = crate::fastswap::models::Device {
                id: peer.id.clone(),
                alias: peer.name.clone(),
                device_model: peer.hostname.clone(),
                device_type: if peer.is_mobile {
                    crate::fastswap::models::DeviceType::Mobile
                } else {
                    crate::fastswap::models::DeviceType::Desktop
                },
                ip: peer.ip.clone(),
                port: if peer.fastswap_port > 0 {
                    peer.fastswap_port
                } else {
                    crate::fastswap::FASTSWAP_TLS_PORT
                },
                protocol: "2.0".to_string(),
                download: true,
            };

            let local = {
                let guard = crate::fastswap::FASTSWAP_DEVICE
                    .lock()
                    .map_err(|_| FfiError::Message("fastswap device lock failed".to_string()))?;
                guard.clone().ok_or_else(|| {
                    FfiError::Message("local fastswap identity not set — call start() first".to_string())
                })?
            };

            let paths: Vec<std::path::PathBuf> =
                files.iter().map(|f| std::path::PathBuf::from(&f.path)).collect();
            let rels: Vec<String> =
                files.iter().map(|f| f.relative_path.clone()).collect();

            let session_id = uuid::Uuid::new_v4().to_string();
            crate::fastswap::register_outbound(&session_id).await;

            let tracker = crate::fastswap::get_progress_tracker();
            let client = crate::fastswap::network::TransferClient::new(tracker).map_err(err)?;
            client
                .send_files(&target, paths, rels, &local, session_id.clone())
                .await
                .map_err(err)?;
            Ok(session_id)
        })
    }

    /// Live progress of all in/out transfer sessions.
    pub fn get_transfer_progress(&self) -> Vec<TransferProgressInfo> {
        self.runtime.block_on(async {
            let tracker = crate::fastswap::get_progress_tracker();
            let guard = tracker.read().await;
            let outbound = crate::fastswap::outbound_sessions().await;
            guard
                .iter()
                .map(|(sid, p)| TransferProgressInfo {
                    session_id: sid.clone(),
                    transferred_bytes: p.transferred_bytes,
                    total_bytes: p.total_bytes,
                    overall_percent: p.overall_progress(),
                    is_complete: p.is_complete(),
                    is_outbound: outbound.contains(sid),
                    file_names: p.files.iter().map(|f| f.file_name.clone()).collect(),
                })
                .collect()
        })
    }

    /// Absolute saved paths of files received in an inbound session (for
    /// MediaStore publish once the session completes).
    pub fn get_received_files(&self, session_id: String) -> Vec<String> {
        self.runtime.block_on(async {
            crate::fastswap::received_files(&session_id)
                .await
                .into_iter()
                .map(|(_, p)| p.to_string_lossy().to_string())
                .collect()
        })
    }

    pub fn get_notifications(&self) -> Vec<IncomingNotification> {
        self.manager
            .lock()
            .map(|g| g.get_notifications().iter().map(notification_info_of).collect())
            .unwrap_or_default()
    }
}

fn bridge_event(listener: &Arc<dyn NodeEventListener>, event: EcoEvent) {
    match event {
        EcoEvent::DeviceDiscovered(dev) | EcoEvent::DeviceConnected(dev)
        | EcoEvent::DeviceDisconnected(dev) | EcoEvent::DeviceTrusted(dev)
        | EcoEvent::DeviceUntrusted(dev) => {
            listener.on_device_updated(device_info_of(&dev));
        }
        EcoEvent::ClipboardReceived(data, _) => {
            listener.on_clipboard_received(clipboard_info_of(&data));
            listener.on_clipboard_received_from(clipboard_info_of(&data));
        }
        EcoEvent::ClipboardChanged(data) | EcoEvent::ClipboardApplied(data) => {
            listener.on_clipboard_received(clipboard_info_of(&data));
        }
        EcoEvent::NotificationReceived(n, _) => {
            listener.on_notification_received(notification_info_of(&n));
        }
        EcoEvent::NotificationReplied(reply) => {
            let key = if reply.notification_key.is_empty() {
                reply.notification_id.clone()
            } else {
                reply.notification_key.clone()
            };
            listener.on_notification_reply_requested(key, reply.reply_text);
        }
        EcoEvent::NotificationDismissed(payload) => {
            listener.on_notification_dismiss_requested(payload.notification_key);
        }
        EcoEvent::NotificationActionRequested(payload) => {
            listener.on_notification_action_requested(payload.notification_key, payload.action_index);
        }
        EcoEvent::Error(msg) | EcoEvent::Synced(msg) => {
            listener.on_log(msg);
        }
        _ => {}
    }
}
