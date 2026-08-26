use crate::fastswap::models::Device;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

const TLS_PORT: u16 = 53318;

pub struct DiscoveryService {
    devices: Arc<RwLock<Vec<Device>>>,
}

impl DiscoveryService {
    pub fn new() -> Self {
        Self {
            devices: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Scan every local interface's /24 for FastSwap peers (see
    /// `eco::discovery::local_subnets` — a single-interface guess goes blind
    /// on multi-homed hosts). Merges and dedups by device id.
    pub async fn scan_all_networks(&self) -> Result<Vec<Device>> {
        let subnets = crate::eco::discovery::local_subnets();
        let mut merged: Vec<Device> = Vec::new();
        for subnet in subnets {
            let found = self.scan_subnet(&subnet).await?;
            for device in found {
                if !merged.iter().any(|d| d.id == device.id) {
                    merged.push(device);
                }
            }
        }

        let mut devices = self.devices.write().await;
        *devices = merged.clone();

        tracing::info!("Scan complete. Found {} devices", merged.len());
        Ok(merged)
    }

    pub async fn scan_network(&self, local_ip: &str) -> Result<Vec<Device>> {
        tracing::info!("Starting network scan from {}", local_ip);

        let mut discovered = Vec::new();

        let parts: Vec<&str> = local_ip.split('.').collect();
        if parts.len() != 4 {
            return Ok(discovered);
        }

        let subnet = format!("{}.{}.{}", parts[0], parts[1], parts[2]);

        let found = self.scan_subnet(&subnet).await?;
        discovered.extend(found);

        let mut devices = self.devices.write().await;
        *devices = discovered.clone();

        tracing::info!("Scan complete. Found {} devices", discovered.len());
        Ok(discovered)
    }

    /// Probe all 254 addresses of one /24 subnet on the FastSwap TLS port.
    async fn scan_subnet(&self, subnet: &str) -> Result<Vec<Device>> {
        let self_ips: std::collections::HashSet<std::net::Ipv4Addr> =
            crate::eco::discovery::local_ipv4s().into_iter().collect();

        let mut discovered = Vec::new();
        let mut tasks = Vec::new();

        for i in 1..=254 {
            let ip = format!("{}.{}", subnet, i);
            let is_self = ip
                .parse::<std::net::Ipv4Addr>()
                .map(|v4| self_ips.contains(&v4))
                .unwrap_or(false);
            if is_self {
                continue;
            }

            let task = tokio::spawn(async move {
                Self::probe_device_https(&ip, TLS_PORT).await
            });

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

        let mut devices = self.devices.write().await;
        *devices = discovered.clone();

        tracing::info!("Scan complete. Found {} devices", discovered.len());
        Ok(discovered)
    }

    async fn probe_device_https(ip: &str, port: u16) -> Option<Device> {
        let url = format!("https://{}:{}/api/localsend/v2/info", ip, port);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .danger_accept_invalid_certs(true)
            .build()
            .ok()?;

        match client.get(&url).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    match response.json::<Device>().await {
                        Ok(mut device) => {
                            device.ip = ip.to_string();
                            device.port = port;
                            tracing::info!("Found device: {} at {}:{} (TLS)", device.alias, ip, port);
                            return Some(device);
                        }
                        Err(e) => {
                            tracing::trace!("Device at {} returned invalid JSON: {}", ip, e);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::trace!("No HTTPS response from {}:{}: {}", ip, port, e);
            }
        }

        None
    }

    pub async fn get_devices(&self) -> Vec<Device> {
        self.devices.read().await.clone()
    }
}
