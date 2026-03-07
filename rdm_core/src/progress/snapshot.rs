use serde::Serialize;

/// Per-segment progress snapshot — pure byte accounting, no speed/ETA.
/// Speed and ETA are computed by the observer layer (e.g. `SseProgressObserver`)
/// and travel on the wire in the enriched type, not here.
#[derive(Debug, Clone, Serialize)]
pub struct SegmentSnapshot {
    pub segment_id: String,
    /// Byte offset of this segment within the file (0 for the first segment).
    pub offset: u64,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
}

/// Aggregate progress snapshot — pure byte accounting, no speed/ETA.
/// Speed and ETA are computed by the observer layer (e.g. `SseProgressObserver`).
#[derive(Debug, Clone, Serialize)]
pub struct ProgressSnapshot {
    pub segments: Vec<SegmentSnapshot>,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub done: bool,
}

impl ProgressSnapshot {
    pub fn empty() -> Self {
        Self {
            segments: Vec::new(),
            total_bytes_downloaded: 0,
            total_bytes: 0,
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
