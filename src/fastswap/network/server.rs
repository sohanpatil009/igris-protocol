use crate::fastswap::models::*;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use futures::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, copy_bidirectional};
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

/// Safe filename — strips path separators and `..` to prevent traversal
fn sanitize_filename(name: &str) -> String {
    let mut safe: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '\0'))
        .collect();
    while safe.contains("..") {
        safe = safe.replace("..", "");
    }
    if safe.trim().is_empty() {
        safe = "unnamed_file".to_string();
    }
    safe
}

/// Safe relative path — allows forward `/` as subdirectory separator but
/// strips backslash, colon, null, and blocks `..` path traversal.
fn sanitize_relative_path(path: &str) -> String {
    let mut safe: String = path
        .chars()
        .filter(|c| !matches!(c, '\\' | ':' | '\0'))
        .collect();
    // Normalize separators
    safe = safe.replace('\\', "/");
    // Collapse repeated slashes
    while safe.contains("//") {
        safe = safe.replace("//", "/");
    }
    // Strip leading/trailing slashes
    let trimmed = safe.trim_matches('/');
    // Split on / and check each segment for `..`
    let segments: Vec<&str> = trimmed.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut clean = Vec::new();
    for seg in segments {
        if seg == ".." {
            clean.pop();
        } else {
            clean.push(seg);
        }
    }
    clean.join("/")
}

/// If `path` exists, append a counter before the extension:
/// `file.txt` → `file (1).txt`, `file (2).txt`, etc.
fn resolve_conflict_path(path: &std::path::Path) -> std::path::PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{}", e))
        .unwrap_or_default();

    for i in 1..1000 {
        let candidate = parent.join(format!("{} ({}){}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
    }
    // fallback — unlikely
    path.to_path_buf()
}

#[derive(Clone)]
pub struct ServerState {
    pub local_device: Arc<RwLock<Device>>,
    pub sessions: Arc<RwLock<Vec<TransferState>>>,
}

pub fn create_router(state: ServerState) -> Router {
    Router::new()
        .route("/api/localsend/v2/info", get(info_handler))
        .route("/api/localsend/v2/register", post(register_handler))
        .route("/api/localsend/v2/prepare-upload", post(prepare_upload_handler))
        .route("/api/localsend/v2/confirm-upload", post(confirm_upload_handler))
        .route("/api/localsend/v2/upload", post(upload_handler))
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::extract::DefaultBodyLimit::max(
                    (10u64 * 1024 * 1024 * 1024).min(usize::MAX as u64) as usize,
                ))
        )
        .with_state(state)
}

async fn info_handler(State(state): State<ServerState>) -> Json<Device> {
    Json(state.local_device.read().await.clone())
}

async fn register_handler(
    State(state): State<ServerState>,
    Json(request): Json<RegisterRequest>,
) -> Json<RegisterResponse> {
    tracing::info!("Device registered: {}", request.alias);

    let device = state.local_device.read().await;

    Json(RegisterResponse {
        alias: device.alias.clone(),
        version: "2.0".to_string(),
        device_model: device.device_model.clone(),
        device_type: device.device_type.clone(),
        fingerprint: device.id.clone(),
    })
}

async fn prepare_upload_handler(
    State(state): State<ServerState>,
    Json(request): Json<PrepareUploadRequest>,
) -> Result<Json<PrepareUploadResponse>, StatusCode> {
    tracing::info!("Preparing upload from {}", request.info.alias);
    tracing::info!("Files to receive: {}", request.files.len());

    let session_id = Uuid::new_v4().to_string();
    let mut file_responses = Vec::new();

    for file in &request.files {
        file_responses.push(FileResponse {
            id: file.id.clone(),
            token: Uuid::new_v4().to_string(),
        });
    }

    let safe_files: Vec<_> = request
        .files
        .iter()
        .map(|f| FileInfo {
            file_name: sanitize_filename(&f.file_name),
            relative_path: sanitize_relative_path(&f.relative_path),
            ..f.clone()
        })
        .collect();

    let pending = crate::fastswap::PendingTransfer {
        session_id: session_id.clone(),
        sender_name: request.info.alias.clone(),
        sender_device: request.info.device_model.clone(),
        file_count: safe_files.len(),
        total_size: safe_files.iter().map(|f| f.size).sum(),
        files: safe_files.iter().map(|f| f.file_name.clone()).collect(),
        created_at: std::time::Instant::now(),
        awaiting_approval: false,
    };

    crate::fastswap::add_pending_transfer(pending).await;
    tracing::info!("Added pending transfer for approval: {}", session_id);

    let transfer_state = TransferState {
        session_id: session_id.clone(),
        files: safe_files
            .iter()
            .map(|f| {
                let save_path = if f.relative_path.is_empty() {
                    std::path::PathBuf::from(&f.file_name)
                } else {
                    std::path::PathBuf::from(&f.relative_path).join(&f.file_name)
                };
                FileTransfer {
                    id: f.id.clone(),
                    name: f.file_name.clone(),
                    path: save_path,
                    size: f.size,
                    transferred: 0,
                    token: file_responses
                        .iter()
                        .find(|fr| fr.id == f.id)
                        .map(|fr| fr.token.clone()),
                }
            })
            .collect(),
        total_size: safe_files.iter().map(|f| f.size).sum(),
        transferred: 0,
        status: TransferStatus::Preparing,
        confirmed: false,
        created_at: std::time::Instant::now(),
    };

    state.sessions.write().await.push(transfer_state);

    Ok(Json(PrepareUploadResponse {
        session_id,
        files: file_responses,
    }))
}

async fn confirm_upload_handler(
    State(state): State<ServerState>,
    Json(request): Json<ConfirmUploadRequest>,
) -> Result<Json<ConfirmUploadResponse>, StatusCode> {
    tracing::info!("Confirming upload for session: {}", request.session_id);

    let notify = crate::fastswap::register_approval_watch(&request.session_id).await;
    crate::fastswap::mark_awaiting(&request.session_id).await;

    // Skip the wait if the user already accepted (accept can land before this
    // request arrives — the popup only shows awaiting sessions, but the click
    // and the HTTP request race).
    if !crate::fastswap::is_transfer_approved(&request.session_id).await {
        let max_wait = std::time::Duration::from_secs(60);

        tracing::info!("Waiting for user approval...");

        let notified = tokio::time::timeout(max_wait, notify.notified()).await;

        if notified.is_err() {
            tracing::warn!("Timeout waiting for approval: {}", request.session_id);
            crate::fastswap::deny_transfer(&request.session_id).await;
            return Err(StatusCode::REQUEST_TIMEOUT);
        }
    }

    let is_approved = crate::fastswap::is_transfer_approved(&request.session_id).await;

    if !is_approved {
        tracing::warn!("Transfer denied by user: {}", request.session_id);
        return Err(StatusCode::FORBIDDEN);
    }

    tracing::info!("Transfer approved by user");

    let mut sessions = state.sessions.write().await;
    let session = sessions
        .iter_mut()
        .find(|s| s.session_id == request.session_id)
        .ok_or_else(|| {
            tracing::error!("Session not found: {}", request.session_id);
            StatusCode::NOT_FOUND
        })?;

    session.confirmed = true;
    session.status = TransferStatus::Transferring;

    let file_progresses: Vec<FileProgress> =
        session.files.iter().map(|f| FileProgress::new(f.id.clone(), f.name.clone(), f.size)).collect();

    let transfer_progress = TransferProgress::new(request.session_id.clone(), file_progresses);
    let tracker = crate::fastswap::get_progress_tracker();
    tracker
        .write()
        .await
        .insert(request.session_id.clone(), transfer_progress);

    tracing::info!(
        "Upload confirmed for session: {} - Progress tracking initialized",
        request.session_id
    );

    crate::fastswap::clear_approved(&request.session_id).await;

    Ok(Json(ConfirmUploadResponse {
        status: "ready".to_string(),
    }))
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "fileId")]
    file_id: String,
    token: String,
}

async fn upload_handler(
    State(state): State<ServerState>,
    Query(query): Query<UploadQuery>,
    body: axum::body::Body,
) -> Result<StatusCode, StatusCode> {
    tracing::info!("Receiving file: {}", query.file_id);

    let download_dir = crate::fastswap::fastswap_downloads_dir();

    let relative_path;
    let file_name;
    let file_size;
    let expected_token;
    {
        let sessions = state.sessions.read().await;
        let session = sessions
            .iter()
            .find(|s| s.session_id == query.session_id)
            .ok_or_else(|| {
                tracing::error!("Session not found: {}", query.session_id);
                StatusCode::NOT_FOUND
            })?;

        if !session.confirmed {
            tracing::error!("Session not confirmed: {}", query.session_id);
            return Err(StatusCode::PRECONDITION_FAILED);
        }

        let file = session
            .files
            .iter()
            .find(|f| f.id == query.file_id)
            .ok_or_else(|| {
                tracing::error!("File not found in session: {}", query.file_id);
                StatusCode::NOT_FOUND
            })?;

        expected_token = file.token.clone();
        relative_path = file.path.clone();
        file_name = file.name.clone();
        file_size = file.size;
    }

    if let Some(ref expected) = expected_token {
        if expected != &query.token {
            tracing::error!("Invalid token for file {}", query.file_id);
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let has_subdir = relative_path
        .parent()
        .map(|p| !p.as_os_str().is_empty())
        .unwrap_or(false);
    let save_path = if has_subdir {
        let dir = download_dir.join(relative_path.parent().unwrap());
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            tracing::error!("Failed to create directory {:?}: {}", dir, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        dir.join(relative_path.file_name().unwrap())
    } else {
        download_dir.join(&file_name)
    };
    let file_path = resolve_conflict_path(&save_path);
    tracing::info!("Saving file to: {:?}", file_path);

    let mut file = tokio::fs::File::create(&file_path)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create file: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut stream = body.into_data_stream();
    let mut total_bytes: u64 = 0;
    let tracker = crate::fastswap::get_progress_tracker();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        file.write_all(&chunk).await.map_err(|e| {
            tracing::error!("Failed to write chunk: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        total_bytes += chunk.len() as u64;

        {
            let mut guard = tracker.write().await;
            if let Some(p) = guard.get_mut(&query.session_id) {
                p.update_file_progress(&query.file_id, total_bytes);
            }
        }
    }

    file.flush().await.map_err(|e| {
        tracing::error!("Failed to flush file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    {
        let mut guard = tracker.write().await;
        if let Some(p) = guard.get_mut(&query.session_id) {
            p.mark_file_completed(&query.file_id);
        }
    }

    if total_bytes != file_size {
        tracing::warn!(
            "File size mismatch for {}: expected {}, written {}",
            file_name,
            file_size,
            total_bytes
        );
    }

    crate::fastswap::record_received_file(&query.session_id, &file_name, &file_path).await;
    tracing::info!("File saved: {:?} ({} bytes)", file_path, total_bytes);
    Ok(StatusCode::OK)
}

pub async fn start_server(
    port: u16,
    local_device: Device,
) -> Result<u16, Box<dyn std::error::Error>> {
    let device = Arc::new(RwLock::new(local_device.clone()));
    let state = ServerState {
        local_device: device.clone(),
        sessions: Arc::new(RwLock::new(Vec::new())),
    };

    let app = create_router(state.clone());

    let cleanup_sessions = state.sessions.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let tracker = crate::fastswap::get_progress_tracker();
            let tracker_guard = tracker.read().await;
            let mut sessions = cleanup_sessions.write().await;
            let before = sessions.len();
            sessions.retain(|s| {
                match s.status {
                    // Confirm never arrived (or was denied/timed out) —
                    // a Preparing session is only valid while the 60 s
                    // approval window is open.
                    TransferStatus::Preparing => {
                        s.created_at.elapsed() < std::time::Duration::from_secs(120)
                    }
                    // Keep only sessions with a live, unfinished transfer.
                    TransferStatus::Transferring => tracker_guard
                        .get(&s.session_id)
                        .map(|p| !(p.is_complete() || p.is_cancelled))
                        .unwrap_or(false),
                    _ => false,
                }
            });
            drop(tracker_guard);
            if before != sessions.len() {
                tracing::info!("Cleaned up {} stale sessions", before - sessions.len());
            }
        }
    });

    for try_port in port..port + 10 {
        let addr = format!("0.0.0.0:{}", try_port);

        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                // Update the device with the actual TLS port (HTTP port + 1, or 53318 if HTTP is 53317)
                let tls_port = if try_port == 53317 { 53318 } else { try_port + 1 };
                {
                    let mut dev = device.write().await;
                    dev.port = tls_port;
                }

                tracing::info!("Server started successfully on HTTP port {}, TLS port {}", try_port, tls_port);
                tracing::info!("Device: {} ({})", local_device.alias, local_device.ip);

                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::error!("Server error: {}", e);
                    }
                });

                return Ok(try_port);
            }
            Err(_e) => {
                if try_port == port {
                    tracing::warn!("Port {} in use, trying alternatives...", try_port);
                }
                continue;
            }
        }
    }

    Err(format!("Could not bind to any port in range {}-{}", port, port + 9).into())
}

/// Runs a TLS proxy that accepts TLS connections on `proxy_port` and forwards
/// decrypted bytes to the HTTP server running on `target_port` (localhost).
pub async fn start_tls_proxy(
    proxy_port: u16,
    target_port: u16,
    server_config: Arc<rustls::ServerConfig>,
) -> Result<u16, Box<dyn std::error::Error>> {
    let acceptor = TlsAcceptor::from(server_config);

    for try_port in proxy_port..proxy_port + 10 {
        let addr = format!("0.0.0.0:{}", try_port);
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                tracing::info!(
                    "TLS proxy on port {}, forwarding to localhost:{}",
                    try_port,
                    target_port,
                );
                tokio::spawn(async move {
                    loop {
                        match listener.accept().await {
                            Ok((stream, _)) => {
                                let acceptor = acceptor.clone();
                                tokio::spawn(async move {
                                    let mut tls_stream = match acceptor.accept(stream).await {
                                        Ok(s) => s,
                                        Err(e) => {
                                            tracing::error!("TLS handshake failed: {}", e);
                                            return;
                                        }
                                    };
                                    let mut local =
                                        match tokio::net::TcpStream::connect(format!(
                                            "127.0.0.1:{}",
                                            target_port
                                        ))
                                        .await
                                        {
                                            Ok(s) => s,
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Cannot connect to HTTP server: {}",
                                                    e,
                                                );
                                                return;
                                            }
                                        };
                                    if let Err(e) =
                                        copy_bidirectional(&mut tls_stream, &mut local).await
                                    {
                                        tracing::debug!("TLS proxy connection closed: {}", e);
                                    }
                                });
                            }
                            Err(e) => tracing::error!("Accept error on TLS proxy: {}", e),
                        }
                    }
                });
                return Ok(try_port);
            }
            Err(_) => continue,
        }
    }

    Err(format!(
        "Could not bind TLS proxy in range {}-{}",
        proxy_port,
        proxy_port + 9
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_removes_path_separators() {
        assert_eq!(sanitize_filename("hello/world.txt"), "helloworld.txt");
        assert_eq!(sanitize_filename("hello\\world.txt"), "helloworld.txt");
    }

    #[test]
    fn test_sanitize_filename_removes_dotdot() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_filename("foo/../bar"), "foobar");
    }

    #[test]
    fn test_sanitize_filename_removes_null() {
        assert_eq!(sanitize_filename("bad\0file.txt"), "badfile.txt");
    }

    #[test]
    fn test_sanitize_filename_allows_normal() {
        assert_eq!(sanitize_filename("normal-file.txt"), "normal-file.txt");
        assert_eq!(sanitize_filename("photo_2024.jpg"), "photo_2024.jpg");
    }

    #[test]
    fn test_sanitize_filename_empty_falls_back() {
        assert_eq!(sanitize_filename(""), "unnamed_file");
        assert_eq!(sanitize_filename("   "), "unnamed_file");
    }

    #[test]
    fn test_sanitize_filename_removes_colon() {
        assert_eq!(
            sanitize_filename("C:\\\\Windows\\file.txt"),
            "CWindowsfile.txt"
        );
    }

    #[test]
    fn test_resolve_conflict_path_no_conflict() {
        let tmp = std::env::temp_dir().join("__fastswap_test_unique_abc.txt");
        let _ = std::fs::remove_file(&tmp);
        let resolved = resolve_conflict_path(&tmp);
        assert_eq!(resolved, tmp);
    }
}
