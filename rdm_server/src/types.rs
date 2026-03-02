use rdm_core::types::types::DownloaderState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
// ---------------------------------------------------------------------------
// Inbound — browser extension payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ExtensionData {
    pub url: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "responseHeaders")]
    pub response_headers: HashMap<String, serde_json::Value>,
    pub method: Option<String>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub file: Option<String>,
    pub filename: Option<String>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<i64>,
    #[serde(rename = "tabUrl")]
    pub tab_url: Option<String>,
    #[serde(rename = "tabId")]
    pub tab_id: Option<String>,
    /// Full absolute output path chosen by the user in the desktop UI.
    /// When present, overrides the auto-derived output path.
    #[serde(rename = "outputPath")]
    pub output_path: Option<String>,
}

/// Payload POSTed by the Dioxus desktop UI on /download.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DownloadRequest {
    pub id: String,
    pub url: String,
    pub title: String,
    #[serde(rename = "outputPath")]
    pub output_path: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    #[serde(default)]
    pub info: String,
}

/// Response returned by POST /download once the download has been queued.
#[derive(Debug, Serialize)]
pub struct DownloadResponse {
    pub id: String,
    pub status: String,
}

/// Payload POSTed by the extension on /media (detected streaming media).
#[derive(Debug, Deserialize)]
pub struct MediaData {
    pub url: String,
    pub file: Option<String>,
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "responseHeaders")]
    pub response_headers: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub cookie: String,
    pub method: Option<String>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    #[serde(rename = "tabUrl")]
    pub tab_url: Option<String>,
    #[serde(rename = "tabId")]
    pub tab_id: Option<String>,
}

/// Payload POSTed by the extension on /tab-update.
#[derive(Debug, Deserialize)]
pub struct TabUpdateData {
    #[serde(rename = "tabUrl")]
    pub tab_url: String,
    #[serde(rename = "tabTitle")]
    pub tab_title: String,
}

/// Payload POSTed by the extension on /vid (user clicked a detected video).
#[derive(Debug, Deserialize)]
pub struct VidRequest {
    pub vid: String,
}

// ---------------------------------------------------------------------------
// Outbound — video list item
// ---------------------------------------------------------------------------

/// A detected streaming video tracked in memory.
/// All fields needed to initiate the download are stored here so that
/// the server can act on a /vid request without contacting the extension again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoListItem {
    pub id: String,
    pub text: String,
    pub info: String,
    #[serde(rename = "tabId")]
    pub tab_id: String,
    pub url: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "responseHeaders")]
    pub response_headers: HashMap<String, serde_json::Value>,
    pub method: Option<String>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    #[serde(rename = "tabUrl")]
    pub tab_url: Option<String>,
    pub referer: Option<String>,
}

impl VideoListItem {
    pub fn get_downloader_state(&self, output_path: String) -> DownloaderState {
        let mut state =
            DownloaderState::new(self.url.clone(), std::path::PathBuf::from(&output_path));
        // Override the auto-generated id with the item's stable id so downloads
        // can be looked up by the same id that the video tracker uses.
        state.id = self.id.clone();
        state.temp_dir = std::env::temp_dir()
            .join(&self.id)
            .to_string_lossy()
            .to_string();
        state.headers = json_headers_to_vec(&self.request_headers);
        if !self.cookie.is_empty() {
            state.cookies = Some(self.cookie.clone());
        }
        state
    }
}
fn json_headers_to_vec(
    headers: &HashMap<String, serde_json::Value>,
) -> HashMap<String, Vec<String>> {
    /// Headers that must be stripped before forwarding to the upstream server.
    fn is_blocked(key: &str) -> bool {
        matches!(key.to_lowercase().as_str(), |"host"| "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "proxy-connection"
            // Managed separately by HeaderData.cookies / apply_headers
            | "cookie"
            // Managed by reqwest (auto-decompression disabled on the client)
            | "accept-encoding"
            // rdm sets its own Range header per segment; a browser-captured Range
            // would create a duplicate and cause the server to return the wrong byte range.
            | "range"
            // Body-related — not relevant for rdm's GET replay
            | "content-length"
            | "content-type")
    }

    headers
        .iter()
        .filter_map(|(k, v)| {
            if is_blocked(k) {
                return None;
            }
            let values: Vec<String> = match v {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|val| val.as_str().map(str::to_string))
                    .collect(),
                serde_json::Value::String(s) => vec![s.clone()],
                _ => return None,
            };
            if values.is_empty() {
                None
            } else {
                Some((k.clone(), values))
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Outbound — sync config (returned by every endpoint)
// ---------------------------------------------------------------------------

/// Payload returned by every endpoint so the extension always has fresh config.
#[derive(Debug, Clone, Serialize)]
pub struct SyncConfig {
    pub enabled: bool,
    #[serde(rename = "fileExts")]
    pub file_exts: Vec<String>,
    #[serde(rename = "blockedHosts")]
    pub blocked_hosts: Vec<String>,
    #[serde(rename = "requestFileExts")]
    pub request_file_exts: Vec<String>,
    #[serde(rename = "mediaTypes")]
    pub media_types: Vec<String>,
    #[serde(rename = "tabsWatcher")]
    pub tabs_watcher: Vec<String>,
    #[serde(rename = "matchingHosts")]
    pub matching_hosts: Vec<String>,
    #[serde(rename = "videoList")]
    pub video_list: Vec<VideoListItem>,
}

impl SyncConfig {
    pub fn default_with_videos(videos: Vec<VideoListItem>) -> Self {
        Self {
            enabled: true,
            file_exts: vec![
                "zip".into(),
                "exe".into(),
                "msi".into(),
                "iso".into(),
                "dmg".into(),
                "pkg".into(),
                "deb".into(),
                "rpm".into(),
                "tar".into(),
                "gz".into(),
                "bz2".into(),
                "7z".into(),
                "rar".into(),
                "pdf".into(),
                "mp4".into(),
                "mkv".into(),
                "avi".into(),
                "mov".into(),
                "wmv".into(),
                "mp3".into(),
                "flac".into(),
                "ogg".into(),
                "wav".into(),
            ],
            blocked_hosts: vec![],
            request_file_exts: vec![
                "mp4".into(),
                "m3u8".into(),
                "m4s".into(),
                "ts".into(),
                "webm".into(),
                "m4v".into(),
                "mpd".into(),
            ],
            media_types: vec!["audio/".into(), "video/".into()],
            tabs_watcher: vec![
                ".youtube.".into(),
                "/watch?v=".into(),
                "vimeo.com".into(),
                "dailymotion.com".into(),
            ],
            matching_hosts: vec!["googlevideo.com".into(), "videoplayback".into()],
            video_list: videos,
        }
    }
}
