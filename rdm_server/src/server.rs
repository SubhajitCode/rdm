use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::RwLock;

use crate::types::{ExtensionData, VideoListItem};
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
    Router::new()
        .route("/download", post(download_handler))
        .route("/status/{id}", get(status_handler))
        .route("/cancel/{id}", post(cancel_handler))
        .route("/videos", get(videos_handler))
        .route("/videos/{id}", post(add_video_handler))
        .route("/videos/{id}", axum::routing::delete(remove_video_handler))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /download
/// Accepts an ExtensionData payload and queues a download.
async fn download_handler(
    State(_state): State<Arc<AppState>>,
    Json(data): Json<ExtensionData>,
) -> Json<serde_json::Value> {
    log::info!("download requested: {}", data.url);
    // TODO: enqueue into download queue (rdm_core::HttpDownloader)
    Json(serde_json::json!({ "status": "queued", "url": data.url }))
}

/// GET /status/:id
/// Returns the current status of a download by ID.
async fn status_handler(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    log::info!("status requested for id={}", id);
    // TODO: look up download queue by id
    Json(serde_json::json!({ "id": id, "status": "unknown" }))
}

/// POST /cancel/:id
/// Cancels an in-progress download.
async fn cancel_handler(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    log::info!("cancel requested for id={}", id);
    // TODO: trigger cancellation token for this download
    Json(serde_json::json!({ "id": id, "status": "cancelled" }))
}

/// GET /videos
/// Returns the list of videos detected by the browser extension.
async fn videos_handler(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<VideoListItem>> {
    let tracker = state.video_tracker.read().await;
    Json(tracker.get_list())
}

/// POST /videos/:id
/// Browser extension pushes a newly detected video.
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
/// Removes a video from the tracked list.
async fn remove_video_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let mut tracker = state.video_tracker.write().await;
    tracker.remove(&id);
    log::info!("video removed: id={}", id);
    Json(serde_json::json!({ "status": "ok" }))
}
