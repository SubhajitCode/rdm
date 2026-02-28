use serde::Serialize;

/// Per-piece progress snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct PieceSnapshot {
    pub piece_id: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
}

/// Aggregate progress snapshot for an entire download.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressSnapshot {
    pub pieces: Vec<PieceSnapshot>,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
    pub done: bool,
}

impl ProgressSnapshot {
    pub fn empty() -> Self {
        Self {
            pieces: Vec::new(),
            total_bytes_downloaded: 0,
            total_bytes: 0,
            speed: 0.0,
            eta_secs: 0.0,
            done: false,
        }
    }
}

/// Human-readable byte formatting.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}
