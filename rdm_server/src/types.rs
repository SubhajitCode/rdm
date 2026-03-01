use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Inbound — browser extension payloads
// ---------------------------------------------------------------------------

/// Full payload POSTed by the extension on /download.
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
    /// Suggested filename (from tab title or Content-Disposition).
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
/// Contains the video item details and the user-chosen output path.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DownloadRequest {
    /// Video item ID (hash of URL, used to key the download in AppState).
    pub id: String,
    /// Media URL to download.
    pub url: String,
    /// Human-readable title (used in UI and for the default filename).
    pub title: String,
    /// Full absolute path where the file should be saved.
    #[serde(rename = "outputPath")]
    pub output_path: String,
    /// Cookie string, if any.
    #[serde(default)]
    pub cookie: String,
    /// Request headers captured by the browser extension.
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    /// Optional User-Agent.
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    /// Optional Referer.
    pub referer: Option<String>,
    /// Content-Type / mime info string.
    #[serde(default)]
    pub info: String,
}

/// Response returned by POST /download once the download has been queued.
#[derive(Debug, Serialize)]
pub struct DownloadResponse {
    /// The download ID — use this to subscribe to GET /progress/{id} and POST /cancel/{id}.
    pub id: String,
    pub status: String,
}

/// Payload POSTed by the extension on /media (detected streaming media).
#[derive(Debug, Deserialize)]
pub struct MediaData {
    pub url: String,
    /// Tab title — used as a human-readable name for the video.
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
    /// Human-readable title (typically the tab title).
    pub text: String,
    /// Extra info shown in the popup (Content-Type, e.g. "video/mp4").
    pub info: String,
    #[serde(rename = "tabId")]
    pub tab_id: String,

    // ── Download data ─────────────────────────────────────────────────────────
    /// The media URL to download.
    pub url: String,
    /// Cookie string extracted from the original request headers.
    #[serde(default)]
    pub cookie: String,
    /// Request headers captured by the extension (e.g. User-Agent, Referer).
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    /// Response headers captured by the extension (e.g. Content-Type, Content-Length).
    #[serde(default, rename = "responseHeaders")]
    pub response_headers: HashMap<String, serde_json::Value>,
    /// HTTP method (usually GET).
    pub method: Option<String>,
    /// User-Agent string from the browser.
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    /// URL of the tab that triggered this media request.
    #[serde(rename = "tabUrl")]
    pub tab_url: Option<String>,
    /// Referer header value, if present in request headers.
    pub referer: Option<String>,
}

// ---------------------------------------------------------------------------
// Outbound — sync config (returned by every endpoint)
// ---------------------------------------------------------------------------

/// Payload returned by every endpoint so the extension always has fresh config.
#[derive(Debug, Clone, Serialize)]
pub struct SyncConfig {
    /// Whether rdm's monitoring is active.
    pub enabled: bool,
    /// File extensions that trigger download takeover (.zip, .iso, …).
    #[serde(rename = "fileExts")]
    pub file_exts: Vec<String>,
    /// Hosts that should never be intercepted.
    #[serde(rename = "blockedHosts")]
    pub blocked_hosts: Vec<String>,
    /// URL-path extensions that indicate streaming media (mp4, m3u8, …).
    #[serde(rename = "requestFileExts")]
    pub request_file_exts: Vec<String>,
    /// Content-Type prefixes that match media (audio/, video/).
    #[serde(rename = "mediaTypes")]
    pub media_types: Vec<String>,
    /// URL substrings whose tab-title changes are reported to /tab-update.
    #[serde(rename = "tabsWatcher")]
    pub tabs_watcher: Vec<String>,
    /// URL substrings that are always captured regardless of content-type.
    #[serde(rename = "matchingHosts")]
    pub matching_hosts: Vec<String>,
    /// Current list of detected videos.
    #[serde(rename = "videoList")]
    pub video_list: Vec<VideoListItem>,
}

impl SyncConfig {
    /// Construct a default config with sensible extension / type lists.
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
