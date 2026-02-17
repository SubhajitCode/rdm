use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentState {
    NotStarted,
    Finished,
    Downloading,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamType {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Piece {
    pub id: String,
    pub offset: i64,
    pub length: i64,
    pub downloaded: i64,
    pub state: SegmentState,
    pub stream_type: StreamType,
}

impl Piece {
    pub fn new(id: String, offset: i64, length: i64) -> Self {
        Self {
            id,
            offset,
            length,
            downloaded: 0,
            state: SegmentState::NotStarted,
            stream_type: StreamType::Primary,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub resumable: bool,
    pub resource_size: Option<u64>,
    pub final_uri: String,
    pub attachment_name: Option<String>,
    pub content_type: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderData {
    pub headers: HashMap<String, Vec<String>>,
    pub cookies: Option<String>,
    pub url: String,
    pub authentication: Option<AuthenticationInfo>,
    pub proxy: Option<ProxyInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticationInfo {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloaderState {
    pub id: String,
    pub url: String,
    pub temp_dir: String,
    pub file_size: i64,
    pub headers: HashMap<String, Vec<String>>,
    pub cookies: Option<String>,
    pub authentication: Option<AuthenticationInfo>,
    pub proxy: Option<ProxyInfo>,
    pub convert_to_mp3: bool,
    pub last_modified: Option<String>,
    pub resumable: bool,
    pub attachment_name: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("disk error: {0}")]
    Disk(#[from] std::io::Error),
    #[error("invalid state")]
    InvalidState,
    #[error("max retry exceeded")]
    MaxRetryExceeded,
    #[error("non-resumable")]
    NonResumable,
    #[error("cancelled")]
    Cancelled,
    #[error("piece failed: {0}")]
    PieceFailed(String),
}

#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub piece_id: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub speed: u64,
    pub progress: u8,
}
