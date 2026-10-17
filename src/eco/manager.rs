use crate::eco::call::{CallManager, CallLogManager};
use crate::eco::clipboard::{ClipboardData, ClipboardManager};
use crate::eco::config::EcosystemConfig;
use crate::eco::constants::*;
use crate::eco::crypto::EcoCrypto;
use crate::eco::device::{Capabilities, EcoDevice};
use crate::eco::discovery::DeviceDiscovery;
use crate::eco::errors::{EcoError, EcoResult};
use crate::eco::events::{EcoEvent, EventBus};
use crate::eco::notification::{NotificationManager, NOTIFICATION_HISTORY};
use crate::eco::permissions::EcoPermissions;
use crate::eco::sms::SmsManager;
use crate::eco::storage::EcoStorage;
use crate::eco::sync::SyncManager;
use crate::eco::transport::EcoTransport;
use crate::platform::ecosystem::PlatformClipboard;
use crate::platform::ecosystem::notifications::PlatformNotification;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct EcoManager {
    config: EcosystemConfig,
    event_bus: Arc<EventBus>,
    transport: Arc<EcoTransport>,
    local_device: Arc<RwLock<EcoDevice>>,
    discovery: Option<Arc<DeviceDiscovery>>,
    clipboard: Option<Arc<std::sync::Mutex<ClipboardManager>>>,
    notification: Option<Arc<std::sync::Mutex<NotificationManager>>>,
    call: Option<Arc<std::sync::Mutex<CallManager>>>,
    call_log: Option<Arc<std::sync::Mutex<CallLogManager>>>,
    sms: Option<Arc<std::sync::Mutex<SmsManager>>>,
    storage: Arc<std::sync::Mutex<EcoStorage>>,
    permissions: Option<Arc<EcoPermissions>>,
    crypto: Option<EcoCrypto>,
    sync: Option<Arc<SyncManager>>,
    eco_port: u16,
    initialized: bool,
    running: bool,
}

impl EcoManager {
    pub fn new(pkg_dir: &PathBuf) -> Self {
        let config_path = pkg_dir.join(ECO_STORAGE_DIR).join(ECO_CONFIG_FILE);
        let config = EcosystemConfig::from_file(&config_path).unwrap_or_default();

        let event_bus = Arc::new(EventBus::new());
        let transport = Arc::new(EcoTransport::new());

        let device_name = config.device_name.clone();
        let local_device = Arc::new(RwLock::new(EcoDevice::new(device_name)));

        let storage = Arc::new(std::sync::Mutex::new(EcoStorage::new(pkg_dir)));

        Self {
            config,
            event_bus,
            transport,
            local_device,
            discovery: None,
            clipboard: None,
            notification: None,
            call: None,
            call_log: None,
            sms: None,
            storage,
            permissions: None,
            crypto: None,
            sync: None,
            eco_port: DEFAULT_ECO_PORT,
            initialized: false,
            running: false,
        }
    }

    pub async fn initialize(
        &mut self,
        pkg_dir: &PathBuf,
        clipboard_platform: Box<dyn PlatformClipboard>,
        notification_platform: Box<dyn PlatformNotification>,
    ) -> EcoResult<()> {
        if self.initialized {
            return Ok(());
        }

        let storage = EcoStorage::new(pkg_dir);
        storage.init_dirs()?;
        let storage = Arc::new(std::sync::Mutex::new(storage));
        // Wire the global pairing manager to this storage so trust persists.
        crate::eco::pairing::init_pairing_manager(storage.clone());
        self.storage = storage;

        let crypto = EcoCrypto::new(pkg_dir)?;
        let public_key_pem = crypto.public_key_pem();

        {
            let mut device = self.local_device.write().await;
            device.public_key = Some(public_key_pem);
            device.capabilities = Capabilities {
                clipboard_sync: self.config.clipboard_sync,
                notification_sync: self.config.notification_sync,
                call_sync: self.config.call_sync,
                sms_sync: self.config.sms_sync,
                clipboard_media: self.config.clipboard_media,
                ..Default::default()
            };

            // Stable identity across restarts (desktop builds). Peers key
            // their trust store by device id; a fresh random UUID per launch
            // silently breaks every existing link until re-paired. The
            // phone's id is managed by its own IdentityStore (injected via
            // the FFI node config) and must NOT be re-generated here.
            #[cfg(not(target_os = "android"))]
            {
                let id_file = pkg_dir.join("device_id");
                match std::fs::read_to_string(&id_file)
                    .ok()
                    .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
                {
                    Some(id) => device.id = id,
                    None => {
                        if let Err(e) = std::fs::write(&id_file, device.id.to_string()) {
                            tracing::warn!("[ECO] could not persist device id: {}", e);
                        }
                    }
                }
            }
        }

        let permissions = EcoPermissions::new(
            EcoStorage::new(pkg_dir)
        );
        self.permissions = Some(Arc::new(permissions));
        self.crypto = Some(crypto);

        let clipboard = ClipboardManager::new(
            clipboard_platform,
            self.event_bus.clone(),
            self.storage.clone(),
        );
        self.clipboard = Some(Arc::new(std::sync::Mutex::new(clipboard)));

        // Initialize notification manager
        let (device_id, device_name) = {
            let device = self.local_device.read().await;
            (device.id.to_string(), device.name.clone())
        };
        let notification = NotificationManager::new(
            notification_platform,
            self.event_bus.clone(),
            pkg_dir,
            device_id,
            device_name,
        );
        self.notification = Some(Arc::new(std::sync::Mutex::new(notification)));

        // Enterprise call + SMS managers (Igris native, persisted history).
        let (device_id2, device_name2) = {
            let device = self.local_device.read().await;
            (device.id.to_string(), device.name.clone())
        };
        let call = CallManager::new(
            self.event_bus.clone(),
            pkg_dir,
            device_id2.clone(),
            device_name2.clone(),
        );
        self.call = Some(Arc::new(std::sync::Mutex::new(call)));
        let call_log = CallLogManager::new();
        self.call_log = Some(Arc::new(std::sync::Mutex::new(call_log)));
        let sms = SmsManager::new(
            self.event_bus.clone(),
            pkg_dir,
            device_id2,
            device_name2,
        );
        self.sms = Some(Arc::new(std::sync::Mutex::new(sms)));

        self.initialized = true;
        self.running = false;

        self.event_bus.emit(EcoEvent::Synced("Ecosystem initialized".to_string()));

        Ok(())
    }

    pub async fn start(&mut self) -> EcoResult<()> {
        if !self.initialized {
            return Err(EcoError::NotInitialized);
        }
        if self.running {
            return Ok(());
        }

        if !self.config.enabled {
            return Ok(());
        }

        // ---- Bind ecosystem HTTP server port ----
        let mut port = self.config.port;
        let mut bound = false;
        for offset in 0..PORT_RANGE {
            let try_port = port + offset;
            let addr: SocketAddr = match format!("0.0.0.0:{}", try_port).parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if tokio::net::TcpListener::bind(addr).await.is_ok() {
                port = try_port;
                bound = true;
                break;
            }
        }
        if !bound {
            return Err(EcoError::Transport("Could not bind to any port".to_string()));
        }
        self.eco_port = port;

        // ---- Start TLS proxy (eco TLS port -> eco HTTP port) ----
        let tls_cfg = tokio::task::spawn_blocking(|| crate::fastswap::tls::get_or_create_tls_config())
            .await
            .map_err(|e| EcoError::Transport(format!("TLS init panicked: {}", e)))?
            .map_err(|e| EcoError::Transport(format!("TLS init error: {}", e)))?;

        let eco_tls_port = if port == DEFAULT_ECO_PORT { ECO_TLS_PORT } else { port + 1 };
        let _ = crate::fastswap::network::start_tls_proxy(eco_tls_port, port, tls_cfg.server_config)
            .await
            .map_err(|e| EcoError::Transport(format!("TLS proxy error: {}", e)))?;

        // ---- Store local device info for pairing ----
        {
            let device = self.local_device.read().await;
            crate::eco::pairing::set_local_device_info(
                device.id.to_string(),
                device.name.clone(),
            );
        }

        // ---- Start ecosystem HTTP server (clipboard endpoint) ----
        let http_addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
        let transport = self.transport.clone();
        let local_device = self.local_device.clone();

        let discovery = Arc::new(DeviceDiscovery::new(
            self.event_bus.clone(),
        ));
        discovery.set_runtime_config(self.config.clone());
        discovery.start_server(&http_addr).await;
        discovery.start_discovery().await;
        discovery.run_cleanup().await;

        let known_devices = discovery.get_known_devices();

        // ---- Sync manager (sends clipboard changes to peers via HTTPS) ----
        let sync = Arc::new(SyncManager::new(
            self.event_bus.clone(),
            transport.clone(),
            known_devices.clone(),
            self.local_device.clone(),
            self.clipboard.clone().unwrap(),
        ));
        let _ = sync.start().await;
        self.sync = Some(sync);

        // ---- Apply received clipboard data to local clipboard ----
        {
            let event_bus = self.event_bus.clone();
            let manager_ptr = self.clipboard.clone().unwrap();
            event_bus.subscribe(Arc::new(move |event| {
                if let EcoEvent::ClipboardReceived(data, _from) = event {
                    let hash_short = if data.content_hash.len() >= 16 { &data.content_hash[..16] } else { &data.content_hash };
                    println!("[ECO] Applying received clipboard (hash={})", hash_short);
                    // Claim synchronously (before any thread hop) so the 1s
                    // poll loop cannot read the incoming text first and echo
                    // it back as a local change.
                    let claimed = manager_ptr
                        .lock()
                        .map(|mut guard| guard.claim_remote(&data))
                        .unwrap_or(false);
                    if !claimed {
                        return;
                    }
                    let manager = manager_ptr.clone();
                    // Use spawn_blocking: std::sync::Mutex::lock() blocks the
                    // current thread; must never run on the tokio runtime.
                    tokio::task::spawn_blocking(move || {
                        let mut guard = match manager.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        // Platform write is blocking I/O (Android JNI) — do it
                        // while holding the lock so no concurrent poll overwrites
                        // our dedup state between check and write.
                        if let Err(e) = guard.write_to_platform(&data) {
                            println!("[ECO] platform write failed: {}", e);
                            guard.mark_failed(&data);
                            return;
                        }
                        guard.mark_applied(&data);
                    });
                }
            }));
        }

        self.discovery = Some(discovery);

        println!("[ECO] HTTP server on port {}, TLS proxy on port {}", port, eco_tls_port);
        println!("[ECO] Discovery via subnet scan on ports {} (HTTP) / {} (TLS)", port, eco_tls_port);

        if self.config.clipboard_sync {
            println!("[ECO] Clipboard monitoring ENABLED (polling every 1s)");
            if let Some(clipboard) = &self.clipboard {
                let cb = clipboard.clone();
                tokio::spawn(async move {
                    ClipboardManager::start_monitoring(cb).await;
                });
            }
        }

        // ---- Notification sync ----
        if self.config.notification_sync {
            println!("[ECO] Notification sync ENABLED (polling every 2s)");
            let notification = self.notification.clone();
            let event_bus = self.event_bus.clone();
            let transport = self.transport.clone();
            let known_devices = self.discovery.as_ref().map(|d| d.get_known_devices());

            // Subscribe to incoming notifications from peers
            {
                let notification = notification.clone();
                event_bus.subscribe(Arc::new(move |event| {
                    if let EcoEvent::NotificationReceived(notif, from) = event {
                        if from == "remote" {
                            println!("[ECO] Received notification from {}: {} - {}",
                                notif.device_name, notif.app_name, notif.title);
                            if let Some(ref mgr) = notification {
                                if let Ok(mut guard) = mgr.lock() {
                                    guard.receive_remote(notif);
                                }
                            }
                        }
                    }
                }));
            }

            // Subscribe to remote dismissals: remove from local history so
            // the mirror + UI follow the phone's swipe-away.
            {
                let notification = notification.clone();
                event_bus.subscribe(Arc::new(move |event| {
                    if let EcoEvent::NotificationDismissed(payload) = event {
                        if let Some(ref mgr) = notification {
                            if let Ok(mut guard) = mgr.lock() {
                                guard.remove_remote(&payload.source_device_id, &payload.notification_key);
                            }
                        }
                    }
                }));
            }

            // Poll local notifications periodically
            let notification_poll = notification.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(
                    std::time::Duration::from_secs(2)
                );
                loop {
                    interval.tick().await;
                    if let Some(ref mgr) = notification_poll {
                        let new_notifs = {
                            if let Ok(mut guard) = mgr.lock() {
                                guard.poll_local()
                            } else {
                                vec![]
                            }
                        };
                        // Sync new notifications to peers (trusted only)
                        for notif in new_notifs {
                            if let Some(ref devices) = known_devices {
                                let devices = devices.read().await;
                                let local_device = crate::eco::pairing::get_local_device_id()
                                    .unwrap_or_default();
                                let trusted: std::collections::HashSet<String> = {
                                    let net = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                                    net.iter()
                                        .filter(|d| d.is_trusted)
                                        .map(|d| d.id.clone())
                                        .collect()
                                };
                                for (id, device) in devices.iter() {
                                    if !trusted.contains(id) {
                                        continue;
                                    }
                                    if let Some(addr) = device.device.addr {
                                        let payload = crate::eco::protocol::NotificationSyncPayload {
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
                                            is_update: false,
                                            source_device_id: local_device.clone(),
                                            source_device_name: notif.device_name.clone(),
                                            timestamp: notif.timestamp,
                                        };
                                        let transport = transport.clone();
                                        tokio::spawn(async move {
                                            let _ = transport.send_notification(&addr, &payload).await;
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            });

            // Update global notification history for UI access
            {
                event_bus.subscribe(Arc::new(move |_event| {
                    if let Some(ref mgr) = notification {
                        if let Ok(guard) = mgr.try_lock() {
                            crate::eco::notification::set_notifications(guard.get_notifications());
                        }
                    }
                }));
            }
        }

        // ---- Enterprise calls + SMS (Igris native, trust-gated) ----
        {
            let call = self.call.clone();
            let call_log = self.call_log.clone();
            let sms = self.sms.clone();
            self.event_bus.subscribe(Arc::new(move |event| {
                match event {
                    EcoEvent::CallReceived(data) => {
                        if let Some(ref mgr) = call {
                            if let Ok(mut guard) = mgr.lock() {
                                guard.receive_remote(data.clone());
                                crate::eco::call::set_calls(guard.get_calls());
                            }
                        }
                    }
                    EcoEvent::CallLogSyncReceived(payload) => {
                        if let Some(ref mgr) = call_log {
                            if let Ok(guard) = mgr.lock() {
                                guard.receive_remote(payload.clone());
                                crate::eco::call::set_call_log(guard.get_log());
                            }
                        }
                    }
                    EcoEvent::SmsReceived(data) => {
                        if let Some(ref mgr) = sms {
                            if let Ok(mut guard) = mgr.lock() {
                                guard.receive_remote(data.clone());
                                crate::eco::sms::set_sms(guard.get_messages());
                            }
                        }
                    }
                    _ => {}
                }
            }));
        }

        // ---- Local call fan-out (host line events -> trusted peers) ----
        // Desktop-only: the phone reports its own calls through the FFI
        // telephony pipe, so this subscription is inert there. Desktop line
        // events carry the local device id and must reach every trusted peer.
        #[cfg(not(target_os = "android"))]
        {
            let transport = self.transport.clone();
            self.event_bus.subscribe(Arc::new(move |event| {
                if let EcoEvent::CallReceived(data) = event {
                    let local_id = crate::eco::pairing::get_local_device_id().unwrap_or_default();
                    if local_id.is_empty() || data.device_id != local_id {
                        return;
                    }
                    let transport = transport.clone();
                    // The poller thread has no tokio runtime — give the
                    // fan-out its own small one (mirrors the toast path).
                    std::thread::spawn(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(_) => return,
                        };
                        rt.block_on(async move {
                            let targets: Vec<SocketAddr> = {
                                let net = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
                                net.iter()
                                    .filter(|d| d.is_trusted)
                                    .filter_map(|d| {
                                        d.ip.parse::<std::net::IpAddr>()
                                            .ok()
                                            .map(|ip| SocketAddr::new(ip, d.port))
                                    })
                                    .collect()
                            };
                            let payload = crate::eco::protocol::CallSyncPayload {
                                call_id: data.call_id.clone(),
                                event: data.event.clone(),
                                phone_number: data.phone_number.clone(),
                                contact_name: data.contact_name.clone(),
                                contact_thumbnail_hash: String::new(),
                                source_device_id: data.device_id.clone(),
                                source_device_name: data.device_name.clone(),
                                timestamp: data.timestamp,
                                is_cancel: data.is_cancel,
                            };
                            for addr in targets {
                                let _ = transport.send_call(&addr, &payload).await;
                            }
                        });
                    });
                }
            }));
        }

        self.running = true;

        self.event_bus.emit(EcoEvent::Synced(
            format!("Ecosystem started — HTTP:{} TLS:{}", port, eco_tls_port)
        ));

        Ok(())
    }

    pub async fn shutdown(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        self.event_bus.emit(EcoEvent::Synced("Ecosystem shut down".to_string()));
    }

    pub fn config(&self) -> &EcosystemConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut EcosystemConfig {
        &mut self.config
    }

    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    /// Discovery handle (None until `start()`). Lets the FFI layer resolve
    /// trusted peer addresses for notification fan-out.
    pub fn discovery(&self) -> Option<Arc<DeviceDiscovery>> {
        self.discovery.clone()
    }

    pub fn clipboard(&self) -> Option<Arc<std::sync::Mutex<ClipboardManager>>> {
        self.clipboard.clone()
    }

    pub fn local_device(&self) -> &Arc<RwLock<EcoDevice>> {
        &self.local_device
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn enable_clipboard_sync(&mut self) {
        self.config.clipboard_sync = true;
    }

    pub fn disable_clipboard_sync(&mut self) {
        self.config.clipboard_sync = false;
    }

    pub fn enable_notification_sync(&mut self) {
        self.config.notification_sync = true;
    }

    pub fn disable_notification_sync(&mut self) {
        self.config.notification_sync = false;
    }

    pub fn notification_manager(&self) -> Option<&Arc<std::sync::Mutex<NotificationManager>>> {
        self.notification.as_ref()
    }

    /// Reply to a notification. Two paths:
    /// - phone (the mirrored notification is a phone-native render backed by
    ///   its RemoteInput channel): platform reply inline;
    /// - desktop (the notification lives on the phone): route the reply over
    ///   the transport to the originating device, then mark the local
    ///   history entry replied.
    pub async fn reply_to_notification(&self, notification_id: &str, reply_text: &str) -> EcoResult<()> {
        #[cfg(target_os = "android")]
        {
            if let Some(ref mgr) = self.notification {
                let guard = mgr.lock().map_err(|_| EcoError::Notification("Lock failed".to_string()))?;
                guard.reply_to_notification(notification_id, reply_text)
            } else {
                Err(EcoError::NotInitialized)
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            if reply_text.trim().is_empty() {
                return Err(EcoError::Notification("Empty reply".to_string()));
            }

            // Locate the notification and its originating device.
            let (notification_key, device_id) = {
                let guard = self.notification
                    .as_ref()
                    .ok_or(EcoError::NotInitialized)?
                    .lock()
                    .map_err(|_| EcoError::Notification("Lock failed".to_string()))?;
                let target = guard
                    .get_notifications()
                    .into_iter()
                    .find(|n| n.id == notification_id || n.notification_key == notification_id);
                match target {
                    Some(n) => (n.notification_key, n.device_id),
                    None => return Err(EcoError::Notification(format!(
                        "No such notification: {}", notification_id
                    ))),
                }
            };

            let addr = self.resolve_peer_addr(&device_id).await.ok_or_else(|| {
                EcoError::Notification(format!(
                    "No route to device {} — is it online and linked?", device_id
                ))
            })?;

            let payload = crate::eco::protocol::NotificationReplyPayload {
                notification_id: notification_id.to_string(),
                notification_key,
                reply_text: reply_text.to_string(),
                source_device_id: crate::eco::pairing::get_local_device_id().unwrap_or_default(),
            };
            self.transport.send_notification_reply(&addr, &payload).await?;

            // Reflect the reply locally.
            if let Some(ref mgr) = self.notification {
                if let Ok(mut guard) = mgr.lock() {
                    guard.mark_replied(notification_id);
                }
            }
            Ok(())
        }
    }

    /// Ask the originating phone to fire one of the notification's action
    /// buttons (v2 wire path).
    pub async fn fire_notification_action(&self, notification_id: &str, action_index: u32) -> EcoResult<()> {
        let (notification_key, device_id) = {
            let guard = self.notification
                .as_ref()
                .ok_or(EcoError::NotInitialized)?
                .lock()
                .map_err(|_| EcoError::Notification("Lock failed".to_string()))?;
            let target = guard
                .get_notifications()
                .into_iter()
                .find(|n| n.id == notification_id || n.notification_key == notification_id);
            match target {
                Some(n) => (n.notification_key, n.device_id),
                None => return Err(EcoError::Notification(format!(
                    "No such notification: {}", notification_id
                ))),
            }
        };

        let addr = self.resolve_peer_addr(&device_id).await.ok_or_else(|| {
            EcoError::Notification(format!(
                "No route to device {} — is it online and linked?", device_id
            ))
        })?;

        let payload = crate::eco::protocol::NotificationActionPayload {
            notification_key,
            action_index,
            source_device_id: crate::eco::pairing::get_local_device_id().unwrap_or_default(),
        };
        self.transport.send_notification_action(&addr, &payload).await
    }

    /// Ask a linked phone to transmit an SMS via its SIM slot (compose
    /// on desktop, transmit on phone — like Continuity handoff).
    pub async fn send_sms_message(
        &self,
        device_id: &str,
        address: &str,
        body: &str,
    ) -> EcoResult<()> {
        let addr = self.resolve_peer_addr(device_id).await.ok_or_else(|| {
            EcoError::Sms(format!("No route to device {}", device_id))
        })?;
        let local_id = crate::eco::pairing::get_local_device_id().unwrap_or_default();
        let payload = crate::eco::protocol::SmsSendPayload {
            address: address.to_string(),
            body: body.to_string(),
            source_device_id: local_id,
        };
        self.transport.request_sms_send(&addr, &payload).await
    }

    /// Search a linked phone's phonebook by name/number fragment for the
    /// dial pad. Trust-gated peer-side; empty when offline or denied.
    pub async fn search_peer_contacts(
        &self,
        device_id: &str,
        query: &str,
    ) -> EcoResult<Vec<crate::platform::ecosystem::ContactEntry>> {
        let addr = self.resolve_peer_addr(device_id).await.ok_or_else(|| {
            EcoError::Call(format!("No route to device {}", device_id))
        })?;
        let local_id = crate::eco::pairing::get_local_device_id().unwrap_or_default();
        self.transport
            .search_contacts(&addr, &local_id, query, 8)
            .await
    }

    pub async fn send_call_action(
        &self,
        device_id: &str,
        call_id: &str,
        action: &str,
        phone_number: &str,
    ) -> EcoResult<()> {
        // Calls living on this host's own line execute through the local
        // telephony hook; everything else is remote control (audio stays on
        // the phone that owns the call).
        let local_id = crate::eco::pairing::get_local_device_id().unwrap_or_default();
        if !local_id.is_empty() && device_id == local_id {
            return crate::platform::ecosystem::hooks::execute_local_call_action(
                call_id,
                action,
                phone_number,
            );
        }
        let addr = self.resolve_peer_addr(device_id).await.ok_or_else(|| {
            EcoError::Call(format!(
                "No route to device {} — is it online and linked?", device_id
            ))
        })?;
        let payload = crate::eco::protocol::CallActionPayload {
            call_id: call_id.to_string(),
            action: action.to_string(),
            source_device_id: local_id,
            phone_number: phone_number.to_string(),
            contact_name: String::new(),
        };
        self.transport
            .send_call_action(&addr, &payload)
            .await
            .map_err(|e| EcoError::Call(e.to_string()))
    }

    /// Push a call-log snapshot to a linked peer so both sides share
    /// history. Trust-gated peer-side.
    pub async fn send_call_log(
        &self,
        device_id: &str,
        log: &[crate::eco::protocol::CallLogEntry],
    ) -> EcoResult<()> {
        let addr = self.resolve_peer_addr(device_id).await.ok_or_else(|| {
            EcoError::Call(format!("No route to device {}", device_id))
        })?;
        let local_id = crate::eco::pairing::get_local_device_id().unwrap_or_default();
        let local_name = crate::eco::pairing::get_local_device_name().unwrap_or_default();
        let payload = crate::eco::protocol::CallLogSyncPayload {
            calls: log.to_vec(),
            source_device_id: local_id,
            source_device_name: local_name,
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        self.transport
            .send_call_log_sync(&addr, &payload)
            .await
            .map_err(|e| EcoError::Call(e.to_string()))
    }

    /// Resolve a trusted peer's address: learned inbound routes first,
    /// live discovery snapshot as fallback (covers restarts where no
    /// inbound request has refreshed the route table yet).
    async fn resolve_peer_addr(&self, device_id: &str) -> Option<SocketAddr> {
        // The discovery leaf keeps the peer's last advertised address,
        // refreshed on every /info heartbeat + registration.
        if let Some(disc) = &self.discovery {
            let devices = disc.get_known_devices();
            let learned = devices
                .read()
                .await
                .get(device_id)
                .and_then(|d| d.device.addr);
            if let Some(addr) = learned {
                return Some(addr);
            }
        }
        let net = crate::eco::discovery::ECO_NETWORK_DEVICES.read().await;
        net.iter().find(|d| d.id == device_id).and_then(|d| {
            d.ip.parse::<std::net::IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, d.port))
        })
    }

    pub fn get_notifications(&self) -> Vec<crate::eco::notification::NotificationData> {
        if let Some(ref mgr) = self.notification {
            if let Ok(guard) = mgr.lock() {
                return guard.get_notifications();
            }
        }
        Vec::new()
    }

    pub fn call_manager(&self) -> Option<Arc<std::sync::Mutex<CallManager>>> {
        self.call.clone()
    }

    pub fn call_log_manager(&self) -> Option<Arc<std::sync::Mutex<CallLogManager>>> {
        self.call_log.clone()
    }

    pub fn sms_manager(&self) -> Option<Arc<std::sync::Mutex<SmsManager>>> {
        self.sms.clone()
    }

    pub fn get_calls(&self) -> Vec<crate::eco::call::CallData> {
        if let Some(ref mgr) = self.call {
            if let Ok(guard) = mgr.lock() {
                return guard.get_calls();
            }
        }
        Vec::new()
    }

    pub fn get_sms(&self) -> Vec<crate::eco::sms::SmsData> {
        if let Some(ref mgr) = self.sms {
            if let Ok(guard) = mgr.lock() {
                return guard.get_messages();
            }
        }
        Vec::new()
    }

    pub fn mark_notification_read(&self, notification_id: &str) {
        if let Some(ref mgr) = self.notification {
            if let Ok(mut guard) = mgr.lock() {
                guard.mark_read(notification_id);
            }
        }
    }

    pub fn clear_notifications(&self) {
        if let Some(ref mgr) = self.notification {
            if let Ok(mut guard) = mgr.lock() {
                guard.clear_all();
            }
        }
    }

    pub async fn apply_received_clipboard(&self, data: ClipboardData) -> EcoResult<()> {
        if let Some(clipboard) = &self.clipboard {
            let mut guard = clipboard.lock().map_err(|_| {
                EcoError::Clipboard("Lock failed".to_string())
            })?;
            if !guard.should_apply(&data) {
                return Ok(());
            }
            guard.write_to_platform(&data)?;
            guard.mark_applied(&data);
            Ok(())
        } else {
            Err(EcoError::NotInitialized)
        }
    }
}

lazy_static::lazy_static! {
    pub static ref ECO_MANAGER: std::sync::Mutex<Option<EcoManager>> = std::sync::Mutex::new(None);
}

pub fn get_eco_manager() -> Option<std::sync::MutexGuard<'static, Option<EcoManager>>> {
    ECO_MANAGER.lock().ok()
}

pub fn init_eco_manager(
    pkg_dir: &PathBuf,
    clipboard_platform: Box<dyn PlatformClipboard>,
    notification_platform: Box<dyn PlatformNotification>,
) -> EcoResult<()> {
    let mut manager = EcoManager::new(pkg_dir);
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| EcoError::Transport(e.to_string()))?;
    runtime.block_on(manager.initialize(pkg_dir, clipboard_platform, notification_platform))?;

    let mut guard = ECO_MANAGER.lock()
        .map_err(|_| EcoError::Transport("Lock failed".to_string()))?;
    *guard = Some(manager);
    Ok(())
}

pub fn start_eco_manager() -> EcoResult<()> {
    let mut guard = ECO_MANAGER.lock()
        .map_err(|_| EcoError::Transport("Lock failed".to_string()))?;
    if let Some(ref mut manager) = *guard {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| EcoError::Transport(e.to_string()))?;
        runtime.block_on(manager.start())?;
        Ok(())
    } else {
        Err(EcoError::NotInitialized)
    }
}

pub async fn init_eco_manager_async(
    pkg_dir: &PathBuf,
    clipboard_platform: Box<dyn PlatformClipboard>,
    notification_platform: Box<dyn PlatformNotification>,
) -> EcoResult<()> {
    let mut manager = EcoManager::new(pkg_dir);
    manager.initialize(pkg_dir, clipboard_platform, notification_platform).await?;
    let mut guard = ECO_MANAGER.lock()
        .map_err(|_| EcoError::Transport("Lock failed".to_string()))?;
    *guard = Some(manager);
    Ok(())
}

pub async fn start_eco_manager_async() -> EcoResult<()> {
    let mut guard = ECO_MANAGER.lock()
        .map_err(|_| EcoError::Transport("Lock failed".to_string()))?;
    if let Some(ref mut manager) = *guard {
        manager.start().await?;
        Ok(())
    } else {
        Err(EcoError::NotInitialized)
    }
}

pub fn shutdown_eco_manager() {
    if let Ok(mut guard) = ECO_MANAGER.lock() {
        if let Some(ref mut manager) = *guard {
            let runtime = tokio::runtime::Runtime::new().ok();
            if let Some(rt) = runtime {
                rt.block_on(manager.shutdown());
            }
        }
    }
}
