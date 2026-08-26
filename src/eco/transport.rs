use crate::eco::constants::*;
use crate::eco::errors::{EcoError, EcoResult};
use crate::eco::protocol::{
    ClipboardSyncPayload, NotificationActionPayload, NotificationDismissPayload,
    NotificationSyncPayload, NotificationReplyPayload,
};
use std::net::SocketAddr;

pub struct EcoTransport {
    http_client: reqwest::Client,
}

impl EcoTransport {
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(PEER_REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();
        Self { http_client }
    }

    /// Send clipboard payload to a peer via HTTPS (FastSwap TLS port).
    pub async fn send_clipboard(
        &self,
        addr: &SocketAddr,
        payload: &ClipboardSyncPayload,
    ) -> EcoResult<()> {
        let url = format!("https://{}/api/ecosystem/v1/clipboard/sync", addr);
        let resp = self.http_client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }

    /// Send notification to a peer via HTTPS.
    pub async fn send_notification(
        &self,
        addr: &SocketAddr,
        payload: &NotificationSyncPayload,
    ) -> EcoResult<()> {
        let url = format!("https://{}/api/ecosystem/v1/notification/sync", addr);
        let resp = self.http_client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }

    /// Send notification reply to a peer via HTTPS.
    pub async fn send_notification_reply(
        &self,
        addr: &SocketAddr,
        payload: &NotificationReplyPayload,
    ) -> EcoResult<()> {
        let url = format!("https://{}/api/ecosystem/v1/notification/reply", addr);
        let resp = self.http_client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }

    /// Tell a peer to dismiss one of its notifications via HTTPS.
    pub async fn send_notification_dismiss(
        &self,
        addr: &SocketAddr,
        payload: &NotificationDismissPayload,
    ) -> EcoResult<()> {
        let url = format!("https://{}/api/ecosystem/v1/notification/dismiss", addr);
        let resp = self.http_client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }

    /// Ask a peer to fire one of its notification's action buttons via HTTPS.
    pub async fn send_notification_action(
        &self,
        addr: &SocketAddr,
        payload: &NotificationActionPayload,
    ) -> EcoResult<()> {
        let url = format!("https://{}/api/ecosystem/v1/notification/action", addr);
        let resp = self.http_client
            .post(&url)
            .json(payload)
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }

    /// Send a pairing request to a peer via HTTPS. The peer trusts us
    /// immediately (direct two-way link) — this is the phone-initiated LINK.
    pub async fn send_pair_request(
        &self,
        addr: &SocketAddr,
        sender_id: &str,
        sender_name: &str,
        sender_port: u16,
    ) -> EcoResult<()> {
        #[derive(serde::Serialize)]
        struct PairRequestPayload {
            sender_id: String,
            sender_name: String,
            sender_port: u16,
        }
        let url = format!("https://{}/api/ecosystem/v1/pair/request", addr);
        let resp = self.http_client
            .post(&url)
            .json(&PairRequestPayload {
                sender_id: sender_id.to_string(),
                sender_name: sender_name.to_string(),
                sender_port,
            })
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }

    /// Tell a peer to stop trusting us (the UNLINK direction). The peer
    /// untrusts the device identified by `device_id` (our own id).
    pub async fn send_untrust(
        &self,
        addr: &SocketAddr,
        device_id: &str,
    ) -> EcoResult<()> {
        #[derive(serde::Serialize)]
        struct UntrustPayload {
            device_id: String,
        }
        let url = format!("https://{}/api/ecosystem/v1/pair/untrust", addr);
        let resp = self.http_client
            .post(&url)
            .json(&UntrustPayload {
                device_id: device_id.to_string(),
            })
            .send()
            .await
            .map_err(|e| EcoError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(EcoError::Transport(format!(
                "Peer returned status {}", resp.status()
            )));
        }

        Ok(())
    }
}
