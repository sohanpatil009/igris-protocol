use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct FileProgress {
    pub file_id: String,
    pub file_name: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub speed: f64, // bytes per second
    pub status: ProgressStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProgressStatus {
    Pending,
    Transferring,
    Completed,
    Failed(String),
    Cancelled,
}

impl FileProgress {
    pub fn new(file_id: String, file_name: String, total_bytes: u64) -> Self {
        Self {
            file_id,
            file_name,
            bytes_sent: 0,
            total_bytes,
            speed: 0.0,
            status: ProgressStatus::Pending,
        }
    }

    pub fn progress_percent(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.bytes_sent as f64 / self.total_bytes as f64) * 100.0
    }

    pub fn eta_seconds(&self) -> Option<u64> {
        if self.speed <= 0.0 || self.bytes_sent >= self.total_bytes {
            return None;
        }
        let remaining = self.total_bytes - self.bytes_sent;
        Some((remaining as f64 / self.speed) as u64)
    }

    pub fn format_speed(&self) -> String {
        format_bytes_per_second(self.speed)
    }

    pub fn format_eta(&self) -> String {
        match self.eta_seconds() {
            Some(seconds) => format_duration(seconds),
            None => "Calculating...".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransferProgress {
    pub session_id: String,
    pub files: Vec<FileProgress>,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub start_time: std::time::Instant,
    pub is_cancelled: bool,
}

impl TransferProgress {
    pub fn new(session_id: String, files: Vec<FileProgress>) -> Self {
        let total_bytes = files.iter().map(|f| f.total_bytes).sum();
        Self {
            session_id,
            files,
            total_bytes,
            transferred_bytes: 0,
            start_time: std::time::Instant::now(),
            is_cancelled: false,
        }
    }

    pub fn overall_progress(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.transferred_bytes as f64 / self.total_bytes as f64) * 100.0
    }

    pub fn overall_speed(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        self.transferred_bytes as f64 / elapsed
    }

    pub fn update_file_progress(&mut self, file_id: &str, bytes_sent: u64) {
        if let Some(file) = self.files.iter_mut().find(|f| f.file_id == file_id) {
            let old_bytes = file.bytes_sent;
            file.bytes_sent = bytes_sent;
            file.status = if bytes_sent >= file.total_bytes {
                ProgressStatus::Completed
            } else {
                ProgressStatus::Transferring
            };

            // Update speed
            let elapsed = self.start_time.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                file.speed = bytes_sent as f64 / elapsed;
            }

            // Update total transferred
            self.transferred_bytes = self.transferred_bytes - old_bytes + bytes_sent;
        }
    }

    pub fn cancel(&mut self) {
        self.is_cancelled = true;
        for file in &mut self.files {
            if file.status == ProgressStatus::Transferring || file.status == ProgressStatus::Pending {
                file.status = ProgressStatus::Cancelled;
            }
        }
    }

    pub fn mark_file_failed(&mut self, file_id: &str, error: String) {
        if let Some(file) = self.files.iter_mut().find(|f| f.file_id == file_id) {
            file.status = ProgressStatus::Failed(error);
        }
    }

    pub fn mark_file_completed(&mut self, file_id: &str) {
        if let Some(file) = self.files.iter_mut().find(|f| f.file_id == file_id) {
            file.bytes_sent = file.total_bytes;
            file.status = ProgressStatus::Completed;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.files.iter().all(|f| matches!(
            f.status,
            ProgressStatus::Completed | ProgressStatus::Failed(_) | ProgressStatus::Cancelled
        ))
    }
}

pub type ProgressTracker = Arc<RwLock<HashMap<String, TransferProgress>>>;

pub fn create_progress_tracker() -> ProgressTracker {
    Arc::new(RwLock::new(HashMap::new()))
}

// Helper functions
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_index])
}

pub fn format_bytes_per_second(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec as u64))
}

pub fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}
