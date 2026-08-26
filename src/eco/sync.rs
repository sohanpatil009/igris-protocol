use crate::eco::clipboard::{ClipboardData, ClipboardManager};
use crate::eco::device::EcoDevice;
use crate::eco::discovery::{DiscoveredDevice, ECO_NETWORK_DEVICES};
use crate::eco::errors::EcoResult;
use crate::eco::events::{EcoEvent, EventBus};
use crate::eco::protocol::ClipboardSyncPayload;
use crate::eco::transport::EcoTransport;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SyncManager {
    event_bus: Arc<EventBus>,
    transport: Arc<EcoTransport>,
    known_devices: Arc<RwLock<HashMap<String, DiscoveredDevice>>>,
    local_device: Arc<RwLock<EcoDevice>>,
    clipboard: Arc<std::sync::Mutex<ClipboardManager>>,
}

impl SyncManager {
    pub(crate) fn new(
        event_bus: Arc<EventBus>,
        transport: Arc<EcoTransport>,
        known_devices: Arc<RwLock<HashMap<String, DiscoveredDevice>>>,
        local_device: Arc<RwLock<EcoDevice>>,
        clipboard: Arc<std::sync::Mutex<ClipboardManager>>,
    ) -> Self {
        Self {
            event_bus,
            transport,
            known_devices,
            local_device,
            clipboard,
        }
    }

    pub(crate) async fn start(&self) -> EcoResult<()> {
        let transport = self.transport.clone();
        let known_devices = self.known_devices.clone();
        let local_device = self.local_device.clone();
        let clipboard = self.clipboard.clone();

        let handler: Arc<dyn Fn(EcoEvent) + Send + Sync> = Arc::new(move |event| {
            if let EcoEvent::ClipboardChanged(data) = event {
                let transport = transport.clone();
                let known_devices = known_devices.clone();
                let local_device = local_device.clone();
                let clipboard = clipboard.clone();
                tokio::spawn(async move {
                    // Only LINKED (trusted) peers receive clipboard updates.
                    let trusted: HashSet<String> = {
                        let net = ECO_NETWORK_DEVICES.read().await;
                        net.iter()
                            .filter(|d| d.is_trusted)
                            .map(|d| d.id.clone())
                            .collect()
                    };

                    // Snapshot target addresses first, then release the lock
                    // before any network I/O.
                    let mut targets: Vec<SocketAddr> = Vec::new();
                    {
                        let peers = known_devices.read().await;
                        for (id, discovered) in peers.iter() {
                            if !discovered.device.capabilities.clipboard_sync {
                                continue;
                            }
                            if !trusted.contains(id) {
                                continue;
                            }
                            if let Some(addr) = discovered.device.addr {
                                targets.push(addr);
                            }
                        }
                    }

                    if targets.is_empty() {
                        tracing::info!(
                            "[ECO] clipboard fan-out: no targets (trusted={:?}, known={})",
                            trusted,
                            known_devices.read().await.len()
                        );
                        return;
                    }

                    let device = local_device.read().await;
                    let sender_id = device.id.to_string();
                    drop(device);

                    let payload = ClipboardSyncPayload {
                        content_hash: data.content_hash.clone(),
                        content: data.content.clone(),
                        content_type: data.content_type.clone(),
                        source_device: sender_id.clone(),
                        timestamp: chrono::Utc::now().timestamp_millis(),
                    };

                    tracing::info!("[ECO] Broadcasting clipboard to {} linked peer(s)", targets.len());
                    let mut delivered = false;
                    for addr in targets {
                        match transport.send_clipboard(&addr, &payload).await {
                            Ok(_) => delivered = true,
                            Err(e) => tracing::info!("[ECO] clipboard send to {} failed: {}", addr, e),
                        }
                    }
                    // Only dedup once the content reached a peer; otherwise an
                    // identical re-push is retried (peer was offline).
                    if delivered {
                        if let Ok(mut guard) = clipboard.lock() {
                            guard.mark_delivered(&data.content_hash);
                        }
                    }
                });
            }
        });

        self.event_bus.subscribe(handler);
        Ok(())
    }

    pub async fn shutdown(&self) {
    }
}
