use crate::eco::config::EcosystemConfig;
use crate::eco::constants::*;
use crate::eco::device::EcoDevice;
use crate::eco::events::{EcoEvent, EventBus};
use crate::eco::protocol::{
    CallActionPayload, CallLogSyncPayload, CallSyncPayload, ClipboardSyncPayload,
    NotificationActionPayload, NotificationDismissPayload, NotificationReplyPayload,
    NotificationSyncPayload, SmsSyncPayload,
};
use axum::extract::ConnectInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredEcoDevice {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub ip: String,
    pub port: u16,
    pub is_trusted: bool,
    pub is_online: bool,
    pub last_seen_secs: u64,
    /// True when the peer's `/info` reported `device_type == "mobile"`.
    #[serde(default)]
    pub is_mobile: bool,
    /// Peer's advertised FastSwap TLS port (from `/info` `fastswap_port`).
    /// 0 = not reported (old binary) → fall back to FASTSWAP_TLS_PORT.
    #[serde(default)]
    pub fastswap_port: u16,
}

#[derive(Deserialize)]
struct PairRequestPayload {
    sender_id: String,
    sender_name: String,
    #[serde(default = "default_port")]
    sender_port: u16,
}

fn default_port() -> u16 {
    ECO_TLS_PORT
}

#[derive(Deserialize)]
struct UntrustPayload {
    device_id: String,
}

lazy_static::lazy_static! {
    pub static ref ECO_NETWORK_DEVICES: Arc<RwLock<Vec<DiscoveredEcoDevice>>> =
        Arc::new(RwLock::new(Vec::new()));
}

/// All IPv4 addresses of usable local interfaces (excludes loopback,
/// link-local and the rmnet 192.0.0/24 internal range). Scanners skip these
/// so a multi-interface host never discovers itself: the legacy single-IP
/// self-skip missed the wlan address whenever `local_ip()` guessed rmnet.
pub fn local_ipv4s() -> Vec<std::net::Ipv4Addr> {
    let mut local_ips: Vec<std::net::Ipv4Addr> = Vec::new();
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in ifaces {
            if let std::net::IpAddr::V4(v4) = ip {
                if !v4.is_loopback() {
                    local_ips.push(v4);
                }
            }
        }
    }
    local_ips
}

/// IPv4 /24 subnets of all usable local interfaces, private-range subnets
/// first, capped at MAX_SCAN_SUBNETS.
///
/// Discovery used to guess one interface via `local_ip()`; on multi-homed
/// hosts (Android rmnet enumerated before wlan0, desktop VPN/vEthernet
/// adapters) it picked the wrong subnet, making that side blind to peers
/// while staying fully discoverable itself — the one-way-visibility bug.
pub fn local_subnets() -> Vec<String> {
    const MAX_SCAN_SUBNETS: usize = 4;

    let mut local_ips = local_ipv4s();
    if local_ips.is_empty() {
        // Legacy fallback: single-interface guess.
        if let Ok(std::net::IpAddr::V4(v4)) = local_ip_address::local_ip() {
            local_ips.push(v4);
        }
    }

    let mut subnets: Vec<(bool, String)> = Vec::new(); // (is_private, "a.b.c")
    for v4 in local_ips {
        let octets = v4.octets();
        // Skip loopback, link-local (169.254/16) and 192.0.0/24 (Qualcomm
        // rmnet internal addressing — never carries LAN peers, and probing
        // its 254 dead addresses wastes ~10s per scan pass).
        if v4.is_loopback()
            || (octets[0] == 169 && octets[1] == 254)
            || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        {
            continue;
        }
        let subnet = format!("{}.{}.{}", octets[0], octets[1], octets[2]);
        if subnets.iter().any(|(_, s)| *s == subnet) {
            continue;
        }
        let is_private = octets[0] == 192 && octets[1] == 168
            || octets[0] == 10
            || octets[0] == 172 && (16..=31).contains(&octets[1]);
        subnets.push((is_private, subnet));
    }
    subnets.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    subnets
        .into_iter()
        .take(MAX_SCAN_SUBNETS)
        .map(|(_, s)| s)
        .collect()
}

pub(crate) struct DiscoveredDevice {
    pub(crate) device: EcoDevice,
    pub(crate) last_heartbeat: Instant,
}

pub struct DeviceDiscovery {
    known_devices: Arc<RwLock<HashMap<String, DiscoveredDevice>>>,
    event_bus: Arc<EventBus>,
    runtime_config: Arc<std::sync::RwLock<crate::eco::config::EcosystemConfig>>,
}

impl DeviceDiscovery {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            known_devices: Arc::new(RwLock::new(HashMap::new())),
            event_bus,
            runtime_config: Arc::new(std::sync::RwLock::new(
                crate::eco::config::EcosystemConfig::default(),
            )),
        }
    }

    pub fn set_runtime_config(&self, config: crate::eco::config::EcosystemConfig) {
        if let Ok(mut guard) = self.runtime_config.write() {
            *guard = config;
        }
    }

    pub(crate) fn get_known_devices(&self) -> Arc<RwLock<HashMap<String, DiscoveredDevice>>> {
        self.known_devices.clone()
    }

    pub async fn start_server(&self, addr: &SocketAddr) {
        let event_bus = self.event_bus.clone();
        let local_id = crate::eco::pairing::get_local_device_id();
        let local_name = crate::eco::pairing::get_local_device_name();
        let runtime_config = self.runtime_config.clone();

        let app = axum::Router::new()
            .route(
                "/api/ecosystem/v1/info",
                axum::routing::get({
                    let dev_id = local_id.clone();
                    let dev_name = local_name.clone();
                    let cfg_ref = runtime_config.clone();
                    move || {
                        let did = dev_id.clone();
                        let dnm = dev_name.clone();
                        let cfg_ref = cfg_ref.clone();
                        async move {
                            let fastswap_tls_port = crate::fastswap::get_fastswap_tls_port();
                            let (cfg_clipboard_sync, cfg_notification_sync, cfg_call_sync, cfg_sms_sync, cfg_clipboard_media) = {
                                let cfg_guard = cfg_ref.read().unwrap();
                                (cfg_guard.clipboard_sync, cfg_guard.notification_sync, cfg_guard.call_sync, cfg_guard.sms_sync, cfg_guard.clipboard_media)
                            };

                            let trusted_peers: Vec<String> = {
                                if let Some(guard) = crate::eco::pairing::get_pairing_manager() {
                                    if let Some(manager) = guard.as_ref() {
                                        manager.get_trusted_ids()
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    Vec::new()
                                }
                            };

                            axum::Json(serde_json::json!({
                                "status": "ok",
                                "ecosystem": true,
                                "clipboard_sync": cfg_clipboard_sync,
                                "notification_sync": cfg_notification_sync,
                                "call_sync": cfg_call_sync,
                                "sms_sync": cfg_sms_sync,
                                "clipboard_media": cfg_clipboard_media,
                                "device_id": did,
                                "device_name": dnm,
                                "device_type": "mobile",
                                "fastswap_port": fastswap_tls_port,
                                "trusted_peers": trusted_peers,
                            }))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/clipboard/sync",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<ClipboardSyncPayload>| {
                        let bus = bus.clone();
                        async move {
                            let payload = body.0;
                            // Enterprise guard: enforce text (1 MiB) and media (10 MiB) limits.
                            if payload.content.len() as u64 > 10 * 1024 * 1024 {
                                return axum::Json(serde_json::json!({"status": "error", "message": "payload too large"}));
                            }
                            let hash = payload.content_hash.clone();
                            let data = crate::eco::clipboard::ClipboardData {
                                content: payload.content,
                                content_type: payload.content_type,
                                content_hash: hash.clone(),
                                source_device: payload.source_device,
                                timestamp: payload.timestamp,
                                encoding: payload.encoding,
                                file_name: payload.file_name,
                                mime_type: payload.mime_type,
                                byte_size: payload.byte_size,
                            };
                            let hash_prefix = if hash.len() >= 8 { &hash[..8] } else { &hash };
                            tracing::info!("[ECO] clipboard/sync received hash={}", hash_prefix);
                            // Fire-and-forget: emit on a Tokio blocking-pool
                            // thread so the HTTP response is returned
                            // immediately. bus.emit() calls all handlers
                            // synchronously (FFI callbacks to Kotlin, lock
                            // acquisitions, etc.) — doing that inline would block
                            // the axum tokio worker, starving the HTTP server
                            // when handlers contend on the same std::sync::Mutex
                            // the clipboard polling loop uses.
                            //
                            // This MUST be `tokio::task::spawn_blocking`, not a
                            // bare `std::thread::spawn`: the clipboard-apply
                            // subscriber in `manager.rs` itself calls
                            // `tokio::task::spawn_blocking`, which panics with
                            // "no reactor running" unless the *calling* thread
                            // already has a Tokio runtime context. A bare OS
                            // thread has none — Tokio's blocking pool does — so
                            // a bare thread here silently dropped every incoming
                            // clipboard push before it ever reached the platform
                            // write. That was the root cause of the PC-to-phone
                            // one-way clipboard sync failure: the HTTP POST
                            // succeeded and the event fired (FFI listener
                            // subscribers saw it fine), but the apply-to-platform
                            // subscriber panicked on this thread before calling
                            // into the Kotlin ClipboardProvider at all.
                            let arc = std::sync::Arc::new(data);
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::ClipboardReceived(arc, String::new()));
                            });
                            tracing::info!("[ECO] clipboard/sync emit spawned");
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/pair/request",
                axum::routing::post({
                    let bus = event_bus.clone();
                    let dev_id = local_id.clone();
                    let dev_name = local_name.clone();
                    move |ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
                          body: axum::extract::Json<PairRequestPayload>| {
                        let bus = bus.clone();
                        let dev_id = dev_id.clone();
                        let dev_name = dev_name.clone();
                        async move {
                            let p = body.0;
                            let sender_id = p.sender_id;
                            let sender_name = p.sender_name;

                            // Direct two-way link: trust the requesting device immediately.
                            let mut eco_network = ECO_NETWORK_DEVICES.write().await;
                            if let Some(dev) = eco_network.iter_mut().find(|d| d.id == sender_id) {
                                dev.is_trusted = true;
                            } else {
                                eco_network.push(DiscoveredEcoDevice {
                                    id: sender_id.clone(),
                                    name: sender_name.clone(),
                                    hostname: String::new(),
                                    ip: remote_addr.ip().to_string(),
                                    port: p.sender_port,
                                    fastswap_port: 0,
                                    is_trusted: true,
                                    is_online: true,
                                    last_seen_secs: 0,
                                    is_mobile: false, // corrected on the next discovery scan
                                });
                            }
                            drop(eco_network);

                            // Persist trust so it survives restarts.
                            crate::eco::pairing::persist_trust(&sender_id);

                            let mut trusted =
                                crate::eco::device::EcoDevice::new(sender_name.clone());
                            trusted.id = uuid::Uuid::parse_str(&sender_id).unwrap_or_else(|_| {
                                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, sender_id.as_bytes())
                            });
                            trusted.addr = Some(remote_addr);

                            bus.emit(EcoEvent::DeviceTrusted(std::sync::Arc::new(trusted)));

                            axum::Json(serde_json::json!({
                                "status": "ok",
                                "trusted": true,
                                "receiver_id": dev_id,
                                "receiver_name": dev_name,
                            }))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/pair/untrust",
                axum::routing::post({
                    move |body: axum::extract::Json<UntrustPayload>| async move {
                        let p = body.0;
                        let mut eco_network = ECO_NETWORK_DEVICES.write().await;
                        for dev in eco_network.iter_mut() {
                            if dev.id == p.device_id {
                                dev.is_trusted = false;
                            }
                        }
                        drop(eco_network);
                        crate::eco::pairing::persist_untrust(&p.device_id);
                        axum::Json(serde_json::json!({"status": "ok"}))
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/notification/sync",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<NotificationSyncPayload>| {
                        let bus = bus.clone();
                        async move {
                            let payload = body.0;
                            if !crate::eco::pairing::is_trusted_sync(&payload.source_device_id) {
                                return axum::Json(
                                    serde_json::json!({"status": "error", "message": "untrusted"}),
                                );
                            }
                            let notif = crate::eco::notification::NotificationData {
                                id: payload.notification_id,
                                notification_key: payload.notification_key,
                                app_package: payload.app_package,
                                app_name: payload.app_name,
                                title: payload.title,
                                body: payload.body,
                                icon: payload.icon,
                                icon_hash: payload.icon_hash,
                                actions: payload.actions,
                                can_reply: payload.can_reply,
                                messages: payload.messages,
                                device_name: payload.source_device_name,
                                device_id: payload.source_device_id,
                                timestamp: payload.timestamp,
                                read: false,
                                replied: false,
                            };
                            let notif_arc = std::sync::Arc::new(notif);
                            // See the clipboard/sync handler above: must stay on
                            // Tokio's blocking pool (not a bare std::thread) so
                            // any subscriber that itself calls spawn_blocking
                            // keeps working.
                            tokio::task::spawn_blocking(move || {
                                // Clone out of the Arc for the event — the event
                                // enum owns its data.
                                let notif = (*notif_arc).clone();
                                bus.emit(EcoEvent::NotificationReceived(
                                    notif,
                                    "remote".to_string(),
                                ));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/notification/reply",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<NotificationReplyPayload>| {
                        let bus = bus.clone();
                        async move {
                            let payload = body.0;
                            let reply = crate::eco::notification::NotificationReply {
                                notification_id: payload.notification_id,
                                notification_key: payload.notification_key,
                                reply_text: payload.reply_text,
                                source_device_id: payload.source_device_id,
                            };
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::NotificationReplied(reply));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/notification/dismiss",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<NotificationDismissPayload>| {
                        let bus = bus.clone();
                        async move {
                            let payload = body.0;
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::NotificationDismissed(payload));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/notification/action",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<NotificationActionPayload>| {
                        let bus = bus.clone();
                        async move {
                            let payload = body.0;
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::NotificationActionRequested(payload));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/notification/request",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<serde_json::Value>| {
                        let bus = bus.clone();
                        async move {
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::NotificationRequested);
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/notification/list",
                axum::routing::get(|| async {
                    axum::Json(crate::eco::notification::get_notifications())
                }),
            )
            .route(
                "/api/ecosystem/v1/call/sync",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<CallSyncPayload>| {
                        let bus = bus.clone();
                        async move {
                            let p = body.0;
                            if !crate::eco::pairing::is_trusted_sync(&p.source_device_id) {
                                return axum::Json(serde_json::json!({"status": "error", "message": "untrusted"}));
                            }
                            let data = crate::eco::call::CallData {
                                call_id: p.call_id,
                                event: p.event,
                                phone_number: p.phone_number,
                                contact_name: p.contact_name,
                                device_name: p.source_device_name,
                                device_id: p.source_device_id,
                                timestamp: p.timestamp,
                                is_cancel: p.is_cancel,
                            };
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::CallReceived(data));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/call/action",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<CallActionPayload>| {
                        let bus = bus.clone();
                        async move {
                            let p = body.0;
                            // Audio routing is a handset decision deferred by
                            // the eco core: the answer action may beat the
                            // OFFHOOK transition, so the request parks in
                            // `eco::audio` and routing fires on the next
                            // local call event instead of executing here.
                            if !crate::eco::audio::peer_audio_request(&p.action) {
                                tokio::task::spawn_blocking(move || {
                                    bus.emit(EcoEvent::CallActionRequested(p));
                                });
                            }
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/call/log/sync",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<CallLogSyncPayload>| {
                        let bus = bus.clone();
                        async move {
                            let p = body.0;
                            if !crate::eco::pairing::is_trusted_sync(&p.source_device_id) {
                                return axum::Json(serde_json::json!({"status": "error", "message": "untrusted"}));
                            }
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::CallLogSyncReceived(p));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/sms/sync",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<SmsSyncPayload>| {
                        let bus = bus.clone();
                        async move {
                            let p = body.0;
                            if !crate::eco::pairing::is_trusted_sync(&p.source_device_id) {
                                return axum::Json(serde_json::json!({"status": "error", "message": "untrusted"}));
                            }
                            let data = crate::eco::sms::SmsData {
                                sms_id: p.sms_id,
                                thread_id: p.thread_id,
                                address: p.address,
                                body: p.body,
                                date: p.date,
                                read: p.read,
                                device_name: p.source_device_name,
                                device_id: p.source_device_id,
                                timestamp: p.timestamp,
                            };
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::SmsReceived(data));
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/call/list",
                axum::routing::get(|| async {
                    axum::Json(crate::eco::call::get_calls())
                }),
            )
            // ---- LAN audio link (desktop host seam; inert when unmapped) ----
            .route(
                "/api/ecosystem/v1/audio/status",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({
                        "session": crate::platform::ecosystem::hooks::audio_link_up()
                    }))
                }),
            )
            .route(
                "/api/ecosystem/v1/audio/attach",
                axum::routing::post({
                    move |body: axum::extract::Json<serde_json::Value>| async move {
                        let host = body
                            .0
                            .get("host")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        match host.parse::<std::net::IpAddr>() {
                            Ok(ip) => {
                                match crate::platform::ecosystem::hooks::audio_attach(ip) {
                                    Ok(()) => axum::Json(serde_json::json!({"status": "ok"})),
                                    Err(e) => axum::Json(
                                        serde_json::json!({"status": "error", "message": e}),
                                    ),
                                }
                            }
                            Err(_) => axum::Json(
                                serde_json::json!({"status": "error", "message": "host must be an IP address"}),
                            ),
                        }
                    }
                }),
            )
            .route(
                "/api/ecosystem/v1/audio/detach",
                axum::routing::post(|| async {
                    crate::platform::ecosystem::hooks::audio_detach();
                    axum::Json(serde_json::json!({"status": "ok"}))
                }),
            )
            .route(
                "/api/ecosystem/v1/audio/test/tone",
                axum::routing::post(|| async {
                    let sent = crate::platform::ecosystem::hooks::audio_test_tone();
                    axum::Json(serde_json::json!({"status": if sent { "ok" } else { "no-link" }}))
                }),
            )
            .route(
                "/api/ecosystem/v1/call/log/list",
                axum::routing::get(|| async {
                    axum::Json(crate::eco::call::get_call_log())
                }),
            )
            .route(
                "/api/ecosystem/v1/sms/list",
                axum::routing::get(|| async {
                    axum::Json(crate::eco::sms::get_sms())
                }),
            )
            .route(
                "/api/ecosystem/v1/contacts/search",
                axum::routing::post(
                    |body: axum::extract::Json<
                        crate::eco::protocol::ContactsSearchRequest,
                    >| async move {
                        let req = body.0;
                        if !crate::eco::pairing::is_trusted_sync(&req.requester_id) {
                            return axum::response::Response::builder()
                                .status(axum::http::StatusCode::FORBIDDEN)
                                .body(axum::body::Body::from(
                                    "{\"status\":\"error\",\"message\":\"untrusted\"}",
                                ))
                                .unwrap();
                        }
                        // Enterprise caps: at most 50 hits, fields trimmed so a
                        // hostile phonebook cannot blow up the response.
                        let limit = req.limit.clamp(1, 50);
                        let hits = crate::platform::ecosystem::contacts_search(&req.query, limit)
                            .into_iter()
                            .take(limit as usize)
                            .map(|mut h| {
                                h.name.truncate(100);
                                h.number.truncate(32);
                                h
                            })
                            .collect::<Vec<_>>();
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::OK)
                            .header("Content-Type", "application/json")
                            .body(axum::body::Body::from(
                                serde_json::to_vec(&hits).unwrap_or_default(),
                            ))
                            .unwrap()
                    },
                ),
            )
            .route(
                "/api/ecosystem/v1/sms/send",
                axum::routing::post({
                    let bus = event_bus.clone();
                    move |body: axum::extract::Json<crate::eco::protocol::SmsSendPayload>| {
                        let bus = bus.clone();
                        async move {
                            let p = body.0;
                            if !crate::eco::pairing::is_trusted_sync(&p.source_device_id) {
                                return axum::Json(serde_json::json!({"status": "error", "message": "untrusted"}));
                            }
                            if p.address.trim().is_empty() || p.body.trim().is_empty() {
                                return axum::Json(serde_json::json!({"status": "error", "message": "empty"}));
                            }
                            if p.body.len() > 1600 {
                                return axum::Json(serde_json::json!({"status": "error", "message": "too long"}));
                            }
                            let address = p.address.clone();
                            let text = p.body.clone();
                            tokio::task::spawn_blocking(move || {
                                bus.emit(EcoEvent::SmsSendRequested { address, body: text });
                            });
                            axum::Json(serde_json::json!({"status": "ok"}))
                        }
                    }
                }),
            );

        let bind_addr = *addr;
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(bind_addr).await;
            if let Ok(listener) = listener {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .ok();
            }
        });
    }

    /// Periodically scan every local interface's /24 on eco's dedicated ports
    /// (53327 HTTP, 53328 TLS) to discover peers — mirrors FastSwap's subnet
    /// probing but on eco's own ports, across all subnets (see `local_subnets`).
    pub async fn start_discovery(&self) {
        let known_devices = self.known_devices.clone();
        let event_bus = self.event_bus.clone();
        let local_id = crate::eco::pairing::get_local_device_id();

        tokio::spawn(async move {
            loop {
                let local_ip = local_ip_address::local_ip().unwrap_or(std::net::IpAddr::V4(
                    std::net::Ipv4Addr::new(192, 168, 1, 1),
                ));
                let local_ip_str = local_ip.to_string();
                let subnets = local_subnets();
                let scans = subnets
                    .iter()
                    .map(|subnet| async move {
                        tokio::join!(
                            Self::scan_subnet(subnet, false),
                            Self::scan_subnet(subnet, true),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut http_count = 0usize;
                let mut tls_count = 0usize;
                let mut all_peers: Vec<EcoDevice> = Vec::new();
                for (peers_http, peers_tls) in futures::future::join_all(scans).await {
                    http_count += peers_http.len();
                    tls_count += peers_tls.len();
                    all_peers.extend(peers_http.into_iter().chain(peers_tls));
                }
                // A peer reachable on both ports shows up twice — dedup by id.
                all_peers.sort_by_key(|p| p.id);
                all_peers.dedup_by(|a, b| a.id == b.id);
                let mut devices = known_devices.write().await;
                let mut network_list = ECO_NETWORK_DEVICES.write().await;
                let mut seen_ids = std::collections::HashSet::new();
                let mut new_count = 0usize;
                for peer in all_peers {
                    let id = peer.id.to_string();
                    let now = Instant::now();
                    seen_ids.insert(id.clone());
                    match devices.get_mut(&id) {
                        Some(existing) => {
                            existing.last_heartbeat = now;
                            // Refresh mutable fields so capability/config
                            // changes on the peer are picked up promptly
                            // (e.g. clipboard_sync toggled in runtime config).
                            existing.device.addr = peer.addr;
                            existing.device.capabilities = peer.capabilities.clone();
                            existing.device.platform = peer.platform.clone();
                            existing.device.trusted_peers = peer.trusted_peers.clone();
                        }
                        None => {
                            devices.insert(
                                id.clone(),
                                DiscoveredDevice {
                                    device: peer.clone(),
                                    last_heartbeat: now,
                                },
                            );
                            new_count += 1;
                        }
                    }
                    let ip_str = peer.addr.map(|a| a.ip().to_string()).unwrap_or_default();
                    let port = peer.addr.map(|a| a.port()).unwrap_or(ECO_TLS_PORT);
                    match network_list.iter_mut().find(|d| d.id == id) {
                        Some(entry) => {
                            entry.name = peer.name.clone();
                            entry.hostname = peer.hostname.clone();
                            entry.ip = ip_str;
                            entry.port = port;
                            entry.fastswap_port = peer.fastswap_port;
                            entry.is_online = true;
                            entry.last_seen_secs = 0;
                            let mut trusted = crate::eco::pairing::is_trusted_sync(&id);
                            // Stale trust detection: if we think this peer is
                            // trusted but the peer's /info says it does NOT
                            // trust us back, the peer either restarted (and
                            // its node has not re-announced trust yet) or
                            // genuinely dropped the link. Demote RAM-only so
                            // the UI reflects the real state, but never
                            // persist the demote — a reboot race must not
                            // erase a saved pairing.
                            if trusted {
                                if let Some(ref local) = local_id {
                                    match &peer.trusted_peers {
                                        Some(ids) if !ids.contains(local) => {
                                            trusted = false;
                                            event_bus.emit(EcoEvent::DeviceUntrusted(
                                                std::sync::Arc::new(peer.clone()),
                                            ));
                                        }
                                        None => {} // old binary, skip check
                                        _ => {}
                                    }
                                }
                            }
                            entry.is_trusted = trusted;
                            entry.is_mobile = peer.platform.eq_ignore_ascii_case("mobile");
                        }
                        None => {
                            let mut trusted = crate::eco::pairing::is_trusted_sync(&id);
                            if trusted {
                                if let Some(ref local) = local_id {
                                    match &peer.trusted_peers {
                                        Some(ids) if !ids.contains(local) => {
                                            trusted = false;
                                            event_bus.emit(EcoEvent::DeviceUntrusted(
                                                std::sync::Arc::new(peer.clone()),
                                            ));
                                        }
                                        None => {}
                                        _ => {}
                                    }
                                }
                            }
                            network_list.push(DiscoveredEcoDevice {
                                id: id.clone(),
                                name: peer.name.clone(),
                                hostname: peer.hostname.clone(),
                                ip: ip_str,
                                port,
                                fastswap_port: peer.fastswap_port,
                                is_trusted: trusted,
                                is_online: true,
                                last_seen_secs: 0,
                                is_mobile: peer.platform.eq_ignore_ascii_case("mobile"),
                            });
                        }
                    }
                }
                // Mark peers not seen this pass as offline. Remove stale devices
                // unless they are LINKED, so links survive brief outages.
                network_list.retain_mut(|d| {
                    if seen_ids.contains(&d.id) {
                        return true;
                    }
                    d.last_seen_secs = d.last_seen_secs.saturating_add(HEARTBEAT_INTERVAL_SECS);
                    if d.last_seen_secs >= DEVICE_TIMEOUT_SECS && !d.is_trusted {
                        return false;
                    }
                    d.is_online = false;
                    true
                });
                drop(network_list);
                drop(devices);
                if new_count > 0 || http_count > 0 || tls_count > 0 {
                    println!("[ECO] Discovery: {} eco (http) + {} eco (tls) devices found ({} new) from {}", http_count, tls_count, new_count, local_ip_str);
                }

                tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS)).await;
            }
        });
    }

    /// Probe a single IP on the given port/scheme for the eco info endpoint.
    async fn probe_ip(ip: &str, port: u16, use_tls: bool) -> Option<EcoDevice> {
        let scheme = if use_tls { "https" } else { "http" };
        let url = format!("{}://{}:{}/api/ecosystem/v1/info", scheme, ip, port);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .danger_accept_invalid_certs(use_tls)
            .build()
            .ok()?;

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let device_id = body
                        .get("device_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let device_name = body
                        .get("device_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&format!("eco-{}", ip))
                        .to_string();
                    if device_id.is_empty() {
                        return None;
                    }
                    let uuid = match uuid::Uuid::parse_str(&device_id) {
                        Ok(u) => u,
                        Err(_) => {
                            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, device_id.as_bytes())
                        }
                    };
                    let mut eco = EcoDevice::new(device_name);
                    eco.id = uuid;
                    if let Ok(ip_addr) = ip.parse::<std::net::IpAddr>() {
                        eco.addr = Some(SocketAddr::new(ip_addr, port));
                    }
                    eco.hostname = body
                        .get("device_model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Carry the peer's self-reported type ("mobile" for phones)
                    // so the keep-alive tier can gate on mobile peers. Overwrites
                    // the EcoDevice::new default (which is the *local* OS) — a
                    // desktop peer omits both keys and stays non-mobile ("").
                    eco.platform = body
                        .get("device_type")
                        .or_else(|| body.get("platform"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    eco.capabilities.clipboard_sync = true;
                    eco.capabilities.notification_sync = body
                        .get("notification_sync")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    eco.capabilities.call_sync = body.get("call_sync").and_then(|v| v.as_bool()).unwrap_or(false);
                    eco.capabilities.sms_sync = body.get("sms_sync").and_then(|v| v.as_bool()).unwrap_or(false);
                    eco.capabilities.clipboard_media = body.get("clipboard_media").and_then(|v| v.as_bool()).unwrap_or(false);
                    // Peer's self-reported trusted device list — used to detect
                    // stale trust (e.g. desktop restarted and lost trust).
                    eco.trusted_peers =
                        body.get("trusted_peers")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            });
                    // Advertised FastSwap TLS port (53318 default; drifts when
                    // 53317 is taken). send_files uses this instead of a
                    // hardcoded constant so transfers survive port drift.
                    eco.fastswap_port = body
                        .get("fastswap_port")
                        .and_then(|v| v.as_u64())
                        .map(|p| p as u16)
                        .unwrap_or(0);
                    Some(eco)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Scan one /24 subnet (`"a.b.c"`) on the ecosystem HTTP (53327) or TLS
    /// (53328) port. Skips every local interface address so the probe host
    /// never discovers itself (see `local_ipv4s`).
    async fn scan_subnet(subnet: &str, use_tls: bool) -> Vec<EcoDevice> {
        let port = if use_tls {
            ECO_TLS_PORT
        } else {
            DEFAULT_ECO_PORT
        };
        let self_ips: std::collections::HashSet<std::net::Ipv4Addr> =
            local_ipv4s().into_iter().collect();

        let mut discovered = Vec::new();
        let mut tasks = Vec::new();

        for i in 1..=254 {
            let ip = format!("{}.{}", subnet, i);
            if let Ok(std::net::IpAddr::V4(v4)) = ip.parse::<std::net::IpAddr>() {
                if self_ips.contains(&v4) {
                    continue;
                }
            }

            let task = tokio::spawn(async move { Self::probe_ip(&ip, port, use_tls).await });
            tasks.push(task);

            if tasks.len() >= 50 {
                for task in tasks.drain(..) {
                    if let Ok(Some(device)) = task.await {
                        discovered.push(device);
                    }
                }
            }
        }
        for task in tasks {
            if let Ok(Some(device)) = task.await {
                discovered.push(device);
            }
        }
        discovered
    }

    pub async fn run_cleanup(&self) {
        let known_devices = self.known_devices.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(DEVICE_TIMEOUT_SECS / 2));
            loop {
                interval.tick().await;
                let mut devices = known_devices.write().await;
                let now = Instant::now();
                devices.retain(|id, d| {
                    let keep = now.duration_since(d.last_heartbeat).as_secs() < DEVICE_TIMEOUT_SECS;
                    if !keep {
                        tracing::info!(
                            "[ECO] cleanup: dropped {} (no heartbeat {}s)",
                            id,
                            DEVICE_TIMEOUT_SECS
                        );
                    }
                    keep
                });
            }
        });
    }
}
