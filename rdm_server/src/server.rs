use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::Method;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

use crate::types::{
    ExtensionData, MediaData, SyncConfig, TabUpdateData, VideoListItem, VidRequest,
};
use crate::video_tracker::VideoTracker;

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub video_tracker: Arc<RwLock<VideoTracker>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            video_tracker: Arc::new(RwLock::new(VideoTracker::new())),
        })
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router(state: Arc<AppState>) -> Router {
    // Allow requests from any chrome-extension:// origin (and localhost for dev).
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers(Any)
        .allow_origin(Any);

    Router::new()
        // ── Extension-facing endpoints (XDM-compatible) ─────────────────────
        .route("/sync",       get(sync_handler))
        .route("/media",      post(media_handler))
        .route("/download",   post(download_handler))
        .route("/tab-update", post(tab_update_handler))
        .route("/vid",        post(vid_handler))
        .route("/clear",      post(clear_handler))
        // ── Internal / REST endpoints ────────────────────────────────────────
        .route("/status/{id}", get(status_handler))
        .route("/cancel/{id}", post(cancel_handler))
        .route("/videos",      get(videos_handler))
        .route("/videos/{id}", post(add_video_handler))
        .route("/videos/{id}", delete(remove_video_handler))
        .route("/echo/{msg}",get(echo_handler))
        .layer(cors)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Helper — build the sync payload with the current video list
// ---------------------------------------------------------------------------

async fn sync_config(state: &Arc<AppState>) -> SyncConfig {
    let tracker = state.video_tracker.read().await;
    SyncConfig::default_with_videos(tracker.get_list())
}

// ---------------------------------------------------------------------------
// Extension-facing handlers
// ---------------------------------------------------------------------------

/// GET /sync
/// Heartbeat + config polling used by the extension's keep-alive alarms.
/// Returns the current SyncConfig so the extension can refresh its state.
async fn sync_handler(State(state): State<Arc<AppState>>) -> Json<SyncConfig> {
    log::debug!("GET /sync");
    Json(sync_config(&state).await)
}

/// POST /media
/// Browser extension detected a streaming media request on a page.
/// Logs the video to the console and stores it in the VideoTracker.
async fn media_handler(
    State(state): State<Arc<AppState>>,
    Json(data): Json<MediaData>,
) -> Json<SyncConfig> {
    // Derive a human-readable title: prefer tab title, fall back to URL.
    let title = data
        .file
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(data.url.as_str())
        .to_string();

    // Derive extra info from response headers if available.
    let content_type = data
        .response_headers
        .get("Content-Type")
        .or_else(|| data.response_headers.get("content-type"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // ── Console log ──────────────────────────────────────────────────────────
    log::info!(
        "[media] title=\"{}\"  url=\"{}\"  type=\"{}\"  tab_url=\"{}\"",
        title,
        data.url,
        content_type,
        data.tab_url.as_deref().unwrap_or("-"),
    );

    // Build a VideoListItem and store it.
    let id = uuid_from_url(&data.url);
    let item = VideoListItem {
        id:     id.clone(),
        text:   title,
        info:   content_type,
        tab_id: data.tab_id.unwrap_or_default(),
    };

    {
        let mut tracker = state.video_tracker.write().await;
        tracker.add_or_update(item);
    }

    let list = {
        let tracker = state.video_tracker.read().await;
        tracker.get_list()
    };

    log::info!("[media] ── video list ({} item{}) ──────────────────────────",
        list.len(), if list.len() == 1 { "" } else { "s" });
    for (i, v) in list.iter().enumerate() {
        log::info!("[media]  {:>2}. [{}]  {}  ({})",
            i + 1, v.info, v.text, v.id);
    }
    log::info!("[media] ─────────────────────────────────────────────────────");

    Json(sync_config(&state).await)
}

/// POST /download
/// Browser extension intercepted a file download and is handing it to rdm.
async fn download_handler(
    State(state): State<Arc<AppState>>,
    Json(data): Json<ExtensionData>,
) -> Json<SyncConfig> {
    let filename = data
        .filename
        .or(data.file.clone())
        .unwrap_or_else(|| filename_from_url(&data.url));

    log::info!(
        "[download] url=\"{}\"  file=\"{}\"  mime=\"{}\"  size={:?}",
        data.url,
        filename,
        data.mime_type.as_deref().unwrap_or("-"),
        data.file_size,
    );
    // TODO: enqueue into rdm_core::HttpDownloader

    Json(sync_config(&state).await)
}

/// POST /tab-update
/// Tab title changed on a watched URL — update matching video entries.
async fn tab_update_handler(
    State(state): State<Arc<AppState>>,
    Json(data): Json<TabUpdateData>,
) -> Json<SyncConfig> {
    log::debug!(
        "[tab-update] tab_url=\"{}\"  title=\"{}\"",
        data.tab_url,
        data.tab_title,
    );

    {
        let mut tracker = state.video_tracker.write().await;
        tracker.update_title_for_tab(&data.tab_url, &data.tab_title);
    }

    Json(sync_config(&state).await)
}

/// POST /vid
/// User clicked a detected video in the popup — trigger its download.
async fn vid_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VidRequest>,
) -> Json<SyncConfig> {
    let result = {
        let tracker = state.video_tracker.read().await;
        tracker.trigger_download(&req.vid)
    };

    match result {
        Ok(msg)  => log::info!("[vid] {}", msg),
        Err(err) => log::warn!("[vid] {}", err),
    }

    Json(sync_config(&state).await)
}

/// POST /clear
/// Clear all detected videos.
async fn clear_handler(State(state): State<Arc<AppState>>) -> Json<SyncConfig> {
    {
        let mut tracker = state.video_tracker.write().await;
        tracker.clear();
    }
    log::info!("[clear] video list cleared");
    Json(sync_config(&state).await)
}

// ---------------------------------------------------------------------------
// Internal REST handlers
// ---------------------------------------------------------------------------

/// GET /status/:id
async fn status_handler(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    log::info!("status requested for id={}", id);
    Json(serde_json::json!({ "id": id, "status": "unknown" }))
}

/// POST /cancel/:id
async fn cancel_handler(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    log::info!("cancel requested for id={}", id);
    Json(serde_json::json!({ "id": id, "status": "cancelled" }))
}

/// GET /videos
async fn videos_handler(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<VideoListItem>> {
    let tracker = state.video_tracker.read().await;
    Json(tracker.get_list())
}

/// POST /videos/:id
async fn add_video_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(item): Json<VideoListItem>,
) -> Json<serde_json::Value> {
    log::info!("video added: id={}", id);
    let mut tracker = state.video_tracker.write().await;
    tracker.add_or_update(item);
    Json(serde_json::json!({ "status": "ok" }))
}

/// DELETE /videos/:id
async fn remove_video_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let mut tracker = state.video_tracker.write().await;
    tracker.remove(&id);
    log::info!("video removed: id={}", id);
    Json(serde_json::json!({ "status": "ok" }))
}

async fn echo_handler(
    State(_state): State<Arc<AppState>>,
    Path(msg): Path<String>,
)  {
    log::info!("echo {}",msg );
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Derive a stable ID from a URL (simple truncated hash).
fn uuid_from_url(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Extract the last path segment from a URL as a filename fallback.
fn filename_from_url(url: &str) -> String {
    url.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("download")
        .to_string()
}
