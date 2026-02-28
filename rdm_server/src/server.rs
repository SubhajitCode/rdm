use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

use rdm_core::downloader::http_downloader::HttpDownloader;
use rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use rdm_core::progress::{ProgressAggregator, ProgressSnapshot};

use crate::path_sanitizer::safe_output_path;
use crate::types::{
    ExtensionData, MediaData, SyncConfig, TabUpdateData, VideoListItem, VidRequest,
};
use crate::video_tracker::VideoTracker;

// ---------------------------------------------------------------------------
// Download tracking
// ---------------------------------------------------------------------------

/// Status of an active or completed download.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Running,
    Complete,
    Failed,
    Cancelled,
}

/// Entry stored in `AppState::downloads` for every dispatched download.
pub struct ActiveDownload {
    pub id:          String,
    pub url:         String,
    pub output_path: PathBuf,
    pub downloader:  Arc<HttpDownloader>,
    pub status:      DownloadStatus,
    pub progress:    Arc<std::sync::RwLock<ProgressSnapshot>>,
}

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub video_tracker: Arc<RwLock<VideoTracker>>,
    /// Active and recently completed downloads, keyed by video id.
    /// TODO migrate to db or any other persistent storage
    pub downloads: Arc<RwLock<HashMap<String, ActiveDownload>>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            video_tracker: Arc::new(RwLock::new(VideoTracker::new())),
            downloads:     Arc::new(RwLock::new(HashMap::new())),
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
        .route("/status/{id}",   get(status_handler))
        .route("/progress/{id}", get(progress_handler))
        .route("/cancel/{id}",   post(cancel_handler))
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

    // Extract Referer from request headers if present.
    let referer = data.request_headers
        .get("Referer")
        .or_else(|| data.request_headers.get("referer"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let item = VideoListItem {
        id:               id.clone(),
        text:             title,
        info:             content_type,
        tab_id:           data.tab_id.clone().unwrap_or_default(),
        url:              data.url.clone(),
        cookie:           data.cookie.clone(),
        request_headers:  data.request_headers.clone(),
        response_headers: data.response_headers.clone(),
        method:           data.method.clone(),
        user_agent:       data.user_agent.clone(),
        tab_url:          data.tab_url.clone(),
        referer,
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
        tracker.get_video(&req.vid)
    };

    match result {
        Ok(item) => {
            log::info!(
                "[vid] triggering download  id=\"{}\"  url=\"{}\"  file=\"{}\"  mime=\"{}\"  cookie=\"{}\"  user_agent=\"{}\"  referer=\"{}\"  tab_url=\"{}\"  method=\"{}\"",
                item.id,
                item.url,
                item.text,
                item.info,
                item.cookie,
                item.user_agent.as_deref().unwrap_or("-"),
                item.referer.as_deref().unwrap_or("-"),
                item.tab_url.as_deref().unwrap_or("-"),
                item.method.as_deref().unwrap_or("GET"),
            );
            spawn_download(item, Arc::clone(&state));
        }
        Err(err) => log::warn!("[vid] {}", err),
    }

    //sync
    Json(sync_config(&state).await)
}

/// Spawn a download task for the given `VideoListItem`.
/// The task runs in the background; the server response is not blocked.
/// The `state` is used to register and update the download's status.
fn spawn_download(item: VideoListItem, state: Arc<AppState>) {
    // Determine a safe output path.
    // `item.info` carries the Content-Type detected by the extension, which
    // helps supply the correct extension when the tab title has none.
    let mime = if item.info.is_empty() { None } else { Some(item.info.as_str()) };
    let output_path = safe_output_path(&item.text, &item.url, mime);
    log::info!("[vid] output_path={:?}", output_path);

    // Convert request headers: HashMap<String, serde_json::Value (array)>
    // → HashMap<String, Vec<String>> as expected by the builder.
    let req_headers = json_headers_to_vec(&item.request_headers);

    // Build the strategy via the builder.
    let builder = MultipartDownloadStrategy::builder(item.url.clone(), output_path.clone())
        .with_headers(req_headers);

    // Set cookies if present.
    let builder = if !item.cookie.is_empty() {
        builder.with_cookies(item.cookie.clone())
    } else {
        builder
    };

    // Inject User-Agent as an explicit header if provided and not already set.
    let builder = if let Some(ua) = &item.user_agent {
        builder.add_header("User-Agent", ua.clone())
    } else {
        builder
    };

    // Inject Referer as an explicit header if provided and not already set.
    let builder = if let Some(referer) = &item.referer {
        builder.add_header("Referer", referer.clone())
    } else {
        builder
    };

    let (strategy, progress_rx) = builder.build();
    let downloader = Arc::new(HttpDownloader::new(Arc::new(strategy)));

    // Create the progress aggregator — drives terminal bars + shared snapshot.
    let (aggregator, progress_snapshot) = ProgressAggregator::new();

    // Register the download in the shared map before spawning.
    let download_id = item.id.clone();
    let download_url = item.url.clone();
    {
        let state_clone = Arc::clone(&state);
        let dl = ActiveDownload {
            id:          download_id.clone(),
            url:         download_url.clone(),
            output_path: output_path.clone(),
            downloader:  Arc::clone(&downloader),
            status:      DownloadStatus::Running,
            progress:    Arc::clone(&progress_snapshot),
        };
        tokio::spawn(async move {
            state_clone.downloads.write().await.insert(dl.id.clone(), dl);
        });
    }

    // Spawn the actual download; update status when done.
    let downloader_clone = Arc::clone(&downloader);
    let state_for_done  = Arc::clone(&state);
    let id_for_done     = download_id.clone();
    let url_for_log     = download_url.clone();
    tokio::spawn(async move {
        let result = downloader_clone.download().await;
        let new_status = match &result {
            Ok(())  => {
                log::info!("[vid] download complete  url=\"{}\"  path={:?}", url_for_log, output_path);
                DownloadStatus::Complete
            }
            Err(e) => {
                log::error!("[vid] download failed  url=\"{}\"  err={:?}", url_for_log, e);
                DownloadStatus::Failed
            }
        };
        // Update status in the map.
        if let Some(entry) = state_for_done.downloads.write().await.get_mut(&id_for_done) {
            entry.status = new_status;
        }
    });

    // Run the progress aggregator — drives indicatif terminal bars and
    // keeps the shared ProgressSnapshot up to date for the SSE endpoint.
    tokio::spawn(async move {
        aggregator.run(progress_rx).await;
    });
}

fn json_headers_to_vec(
    headers: &HashMap<String, serde_json::Value>,
) -> HashMap<String, Vec<String>> {
    /// Headers that must be stripped before forwarding to the upstream server.
    fn is_blocked(key: &str) -> bool {
        matches!(
            key.to_lowercase().as_str(),
            | "host"
            | "connection"
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
            // Managed by piece_grabber — rdm sets its own Range header per piece;
            // a browser-captured Range would create a duplicate and cause the
            // server to return the wrong byte range (or the full file).
            | "range"
            // Body-related — not relevant for rdm's GET replay
            | "content-length"
            | "content-type"
        )
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
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let downloads = state.downloads.read().await;
    if let Some(dl) = downloads.get(&id) {
        Json(serde_json::json!({
            "id":          dl.id,
            "url":         dl.url,
            "output_path": dl.output_path.to_string_lossy(),
            "status":      dl.status,
        }))
    } else {
        Json(serde_json::json!({ "id": id, "status": "not_found" }))
    }
}

/// POST /cancel/:id
async fn cancel_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let mut downloads = state.downloads.write().await;
    if let Some(dl) = downloads.get_mut(&id) {
        match dl.downloader.stop().await {
            Ok(()) => {
                dl.status = DownloadStatus::Cancelled;
                log::info!("[cancel] id={} cancelled", id);
                Json(serde_json::json!({ "id": id, "status": "cancelled" }))
            }
            Err(e) => {
                log::warn!("[cancel] id={} stop error: {:?}", id, e);
                Json(serde_json::json!({ "id": id, "status": "error", "detail": format!("{:?}", e) }))
            }
        }
    } else {
        Json(serde_json::json!({ "id": id, "status": "not_found" }))
    }
}

/// GET /progress/:id — Server-Sent Events stream of download progress.
///
/// Emits a JSON `ProgressSnapshot` every 300ms until the download is done,
/// then sends a final event and closes the stream.
async fn progress_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    // Look up the download and clone the snapshot handle.
    let snapshot = {
        let downloads = state.downloads.read().await;
        let dl = downloads.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        Arc::clone(&dl.progress)
    };

    // Use async_stream to have full control over when the stream ends.
    let stream = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            interval.tick().await;
            let snap = snapshot.read().unwrap().clone();
            let is_done = snap.done;
            let json = serde_json::to_string(&snap).unwrap_or_default();
            yield Ok::<_, Infallible>(Event::default().data(json));
            if is_done {
                break;
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
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
