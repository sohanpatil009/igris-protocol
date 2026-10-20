use crate::fastswap::models::*;
use anyhow::{Context, Result};
use futures::future::join_all;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use uuid::Uuid;

const TLS_PORT_START: u16 = 53318;
const TLS_PORT_END: u16 = 53328;

fn device_url(target: &Device, path: &str) -> String {
    let scheme = if (TLS_PORT_START..TLS_PORT_END).contains(&target.port) {
        "https"
    } else {
        "http"
    };
    format!("{}://{}:{}{}", scheme, target.ip, target.port, path)
}

pub struct TransferClient {
    client: reqwest::Client,
    progress_tracker: ProgressTracker,
}

impl TransferClient {
    pub fn new(progress_tracker: ProgressTracker) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .danger_accept_invalid_certs(true)
                .build()
                .context("Failed to build reqwest client")?,
            progress_tracker,
        })
    }

    pub async fn send_files(
        &self,
        target: &Device,
        files: Vec<PathBuf>,
        relative_dirs: Vec<String>,
        local_device: &Device,
        local_session_id: String,
    ) -> Result<String> {
        tracing::info!("Sending {} files to {}", files.len(), target.alias);

        let mut file_infos = Vec::new();
        let mut file_progresses = Vec::new();

        for (path, rel_dir) in files.iter().zip(relative_dirs.iter()) {
            let metadata = tokio::fs::metadata(path).await?;
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            let file_id = Uuid::new_v4().to_string();

            file_infos.push(FileInfo {
                id: file_id.clone(),
                file_name: file_name.clone(),
                relative_path: rel_dir.clone(),
                size: metadata.len(),
                file_type: mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string(),
                preview: None,
            });

            file_progresses.push(FileProgress::new(file_id, file_name, metadata.len()));
        }

        let transfer_progress = TransferProgress::new(local_session_id.clone(), file_progresses);
        self.progress_tracker
            .write()
            .await
            .insert(local_session_id.clone(), transfer_progress);

        let prepare_request = PrepareUploadRequest {
            info: DeviceInfo {
                alias: local_device.alias.clone(),
                version: "1.0".to_string(),
                device_model: local_device.device_model.clone(),
                device_type: format!("{:?}", local_device.device_type).to_lowercase(),
                fingerprint: local_device.id.clone(),
            },
            files: file_infos.clone(),
        };

        let url = device_url(target, "/api/fastswap/v1/prepare-upload");

        tracing::info!("Preparing upload to {}", url);
        let response = self.client.post(&url).json(&prepare_request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let mut tracker = self.progress_tracker.write().await;
            if let Some(progress) = tracker.get_mut(&local_session_id) {
                for file in &mut progress.files {
                    file.status = ProgressStatus::Failed(format!("Prepare failed: {}", status));
                }
            }
            anyhow::bail!("Failed to prepare upload: {} - {}", status, body);
        }

        let prepare_response: PrepareUploadResponse = response.json().await?;
        let server_session_id = prepare_response.session_id.clone();
        tracing::info!("Upload prepared, server session: {}", server_session_id);

        let confirm_url = device_url(target, "/api/fastswap/v1/confirm-upload");
        let confirm_request = ConfirmUploadRequest {
            session_id: server_session_id.clone(),
        };

        tracing::info!("Sending confirmation handshake...");
        let confirm_response = self
            .client
            .post(&confirm_url)
            .json(&confirm_request)
            .send()
            .await?;

        if !confirm_response.status().is_success() {
            let status = confirm_response.status();
            let body = confirm_response.text().await.unwrap_or_default();
            let mut tracker = self.progress_tracker.write().await;
            if let Some(progress) = tracker.get_mut(&local_session_id) {
                for file in &mut progress.files {
                    file.status = ProgressStatus::Failed(format!("Handshake failed: {}", status));
                }
            }
            anyhow::bail!("Failed to confirm upload: {} - {}", status, body);
        }

        let _confirm_resp: ConfirmUploadResponse = confirm_response.json().await?;
        tracing::info!("Three-way handshake complete, starting transfer");

        // Cancellation channel — UI calls cancel_transfer which toggles this
        let (cancel_tx, cancel_rx) = watch::channel(false);
        crate::fastswap::register_cancellation(&local_session_id, cancel_tx).await;

        // Concurrent uploads with cancellation
        let mut handles = Vec::new();
        for (file_path, file_info) in files.iter().zip(file_infos.iter()) {
            let file_response = prepare_response
                .files
                .iter()
                .find(|f| f.id == file_info.id)
                .ok_or_else(|| anyhow::anyhow!("File response not found"))?;

            let mut cancel_rx: watch::Receiver<bool> = cancel_rx.clone();
            let client = self.client.clone();
            let tracker = self.progress_tracker.clone();
            let local_sid = local_session_id.clone();
            let server_sid = server_session_id.clone();
            let target_device = target.clone();
            let path = file_path.clone();
            let fid = file_info.id.clone();
            let ftoken = file_response.token.clone();
            let fsize = file_info.size;
            let ftype = file_info.file_type.clone();

            handles.push(tokio::spawn(async move {
                // Check cancellation via watch
                if *cancel_rx.borrow_and_update() {
                    let mut guard = tracker.write().await;
                    if let Some(progress) = guard.get_mut(&local_sid) {
                        progress.mark_file_failed(&fid, "Cancelled".into());
                    }
                    return;
                }

                upload_file(
                    &client,
                    &tracker,
                    &target_device,
                    &path,
                    &server_sid,
                    &fid,
                    &ftoken,
                    fsize,
                    &ftype,
                    &local_sid,
                )
                .await;

                let mut guard = tracker.write().await;
                if let Some(progress) = guard.get_mut(&local_sid) {
                    progress.mark_file_completed(&fid);
                }
            }));
        }

        let results = join_all(handles).await;

        // Check for errors
        for r in &results {
            if let Err(e) = r {
                let mut guard = self.progress_tracker.write().await;
                if let Some(progress) = guard.get_mut(&local_session_id) {
                    for file in &mut progress.files {
                        if file.status == ProgressStatus::Transferring
                            || file.status == ProgressStatus::Pending
                        {
                            file.status = ProgressStatus::Failed(format!("Transfer error: {}", e));
                        }
                    }
                }
                tracing::error!("Concurrent upload task failed: {}", e);
            }
        }

        crate::fastswap::unregister_cancellation(&local_session_id).await;

        tracing::info!("All files sent");
        Ok(local_session_id)
    }

    pub async fn cancel_transfer(&self, session_id: &str) {
        let mut tracker = self.progress_tracker.write().await;
        if let Some(progress) = tracker.get_mut(session_id) {
            progress.cancel();
            tracing::info!("Transfer {} cancelled", session_id);
        }
        crate::fastswap::notify_cancellation(session_id).await;
    }
}

async fn upload_file(
    client: &reqwest::Client,
    tracker: &ProgressTracker,
    target: &Device,
    file_path: &PathBuf,
    server_session_id: &str,
    file_id: &str,
    token: &str,
    file_size: u64,
    content_type: &str,
    local_session_id: &str,
) {
    tracing::info!(
        "Uploading file: {:?} ({} bytes / {:.2} MB)",
        file_path,
        file_size,
        file_size as f64 / 1_048_576.0
    );

    // Update status to transferring
    {
        let mut guard = tracker.write().await;
        if let Some(progress) = guard.get_mut(local_session_id) {
            if let Some(file) = progress.files.iter_mut().find(|f| f.file_id == file_id) {
                file.status = ProgressStatus::Transferring;
            }
        }
    }

    let url = device_url(
        target,
        &format!(
            "/api/fastswap/v1/upload?sessionId={}&fileId={}&token={}",
            server_session_id, file_id, token
        ),
    );

    let file = match File::open(file_path).await {
        Ok(f) => f,
        Err(e) => {
            let mut guard = tracker.write().await;
            if let Some(progress) = guard.get_mut(local_session_id) {
                progress.mark_file_failed(file_id, format!("Failed to open file: {}", e));
            }
            return;
        }
    };
    let mut reader = tokio::io::BufReader::new(file);

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(100);

    let tracker_clone = tracker.clone();
    let local_sid = local_session_id.to_string();
    let fid = file_id.to_string();
    let chunk_size = 64 * 1024;

    tokio::spawn(async move {
        let mut bytes_sent = 0u64;
        let mut buffer = vec![0u8; chunk_size];

        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    bytes_sent += n as u64;
                    {
                        let mut guard = tracker_clone.write().await;
                        if let Some(progress) = guard.get_mut(&local_sid) {
                            progress.update_file_progress(&fid, bytes_sent);
                        }
                    }
                    let chunk = bytes::Bytes::copy_from_slice(&buffer[..n]);
                    if tx.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                    if bytes_sent % (chunk_size as u64 * 10) == 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = reqwest::Body::wrap_stream(stream);

    let response = match client
        .post(&url)
        .header("Content-Type", content_type)
        .header("Content-Length", file_size.to_string())
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let mut guard = tracker.write().await;
            if let Some(progress) = guard.get_mut(local_session_id) {
                progress.mark_file_failed(file_id, format!("Upload request failed: {}", e));
            }
            return;
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let mut guard = tracker.write().await;
        if let Some(progress) = guard.get_mut(local_session_id) {
            progress.mark_file_failed(file_id, format!("Upload failed: {} - {}", status, body));
        }
        return;
    }

    tracing::info!("File uploaded: {:?}", file_path);
}
