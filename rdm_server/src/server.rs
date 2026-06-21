use std::collections::HashMap;
use std::convert::Infallible;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tokio::sync::{watch, Mutex as TokioMutex, RwLock};
use tower_http::cors::{Any, CorsLayer};

use rdm_core::downloader::http_downloader::HttpDownloader;
use rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use rdm_core::types::types::DownloadError;

use crate::db::{DownloadDatabase, PersistedDownload};
use crate::path_sanitizer::safe_output_path;
use crate::sse_observer::{EnrichedSnapshot, SseProgressObserver};
use crate::types::{
    DownloadRequest, DownloadResponse, DownloadStatus, DownloadSummary, MediaData, SyncConfig,
    TabUpdateData, VidRequest, VideoListItem,
};
use crate::video_tracker::VideoTracker;

pub struct ActiveDownload {
    pub downloader: Arc<TokioMutex<HttpDownloader>>,
    pub controller: Arc<dyn DownloadStrategy>,
    pub progress_rx: watch::Receiver<EnrichedSnapshot>,
    pub stop_requested: Arc<AtomicBool>,
}

pub struct AppState {
    pub video_tracker: Arc<RwLock<VideoTracker>>,
    pub downloads: Arc<RwLock<HashMap<String, ActiveDownload>>>,
    pub db: DownloadDatabase,
    pub connections: usize,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Self::with_connections(8)
    }

    pub fn with_connections(connections: usize) -> Arc<Self> {
        let db = DownloadDatabase::new(default_db_path())
            .unwrap_or_else(|e| panic!("failed to initialize download database: {}", e));
        if let Err(e) = db.mark_running_as_interrupted() {
            log::warn!("[db] failed to mark interrupted downloads: {}", e);
        }

        Arc::new(Self {
            video_tracker: Arc::new(RwLock::new(VideoTracker::new())),
            downloads: Arc::new(RwLock::new(HashMap::new())),
            db,
            connections,
        })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers(Any)
        .allow_origin(Any);

    Router::new()
        .route("/sync", get(sync_handler))
        .route("/media", post(media_handler))
        .route("/download", post(download_handler))
        .route("/tab-update", post(tab_update_handler))
        .route("/vid", post(vid_handler))
        .route("/clear", post(clear_handler))
        .route("/status/{id}", get(status_handler))
        .route("/progress/{id}", get(progress_handler))
        .route("/cancel/{id}", post(stop_handler))
        .route("/downloads", get(downloads_handler))
        .route("/downloads/{id}", get(download_detail_handler).delete(delete_entry_handler))
        .route("/downloads/{id}/stop", post(stop_handler))
        .route("/downloads/{id}/resume", post(resume_handler))
        .route("/downloads/{id}/files", delete(delete_with_files_handler))
        .route("/videos", get(videos_handler))
        .route("/videos/{id}", post(add_video_handler))
        .route("/videos/{id}", delete(remove_video_handler))
        .route("/echo/{msg}", get(echo_handler))
        .layer(cors)
        .with_state(state)
}

async fn sync_config(state: &Arc<AppState>) -> SyncConfig {
    let tracker = state.video_tracker.read().await;
    SyncConfig::default_with_videos(tracker.get_list())
}

async fn sync_handler(State(state): State<Arc<AppState>>) -> Json<SyncConfig> {
    log::debug!("GET /sync");
    Json(sync_config(&state).await)
}

async fn media_handler(
    State(state): State<Arc<AppState>>,
    Json(data): Json<MediaData>,
) -> Json<SyncConfig> {
    let title = data
        .file
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(data.url.as_str())
        .to_string();

    let content_type = data
        .response_headers
        .get("Content-Type")
        .or_else(|| data.response_headers.get("content-type"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    log::info!(
        "[media] title=\"{}\"  url=\"{}\"  type=\"{}\"  tab_url=\"{}\"",
        title,
        data.url,
        content_type,
        data.tab_url.as_deref().unwrap_or("-"),
    );

    let id = uuid_from_url(&data.url);

    let referer = data.request_headers
        .get("Referer")
        .or_else(|| data.request_headers.get("referer"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(sanitize_header_value);

    let item = VideoListItem {
        id: id.clone(),
        text: title,
        info: content_type,
        tab_id: data.tab_id.clone().unwrap_or_default(),
        url: data.url.clone(),
        cookie: data.cookie.clone(),
        request_headers: data.request_headers.clone(),
        response_headers: data.response_headers.clone(),
        method: data.method.clone(),
        user_agent: data.user_agent.as_deref().map(sanitize_header_value),
        tab_url: data.tab_url.clone(),
        referer,
    };

    {
        let mut tracker = state.video_tracker.write().await;
        tracker.add_or_update(item);
    }

    Json(sync_config(&state).await)
}

async fn download_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DownloadRequest>,
) -> Json<DownloadResponse> {
    log::info!(
        "[download] id=\"{}\"  url=\"{}\"  title=\"{}\"  output_path=\"{}\"",
        req.id,
        req.url,
        req.title,
        req.output_path,
    );

    let item = VideoListItem {
        id: req.id.clone(),
        text: req.title.clone(),
        info: req.info.clone(),
        tab_id: String::new(),
        url: req.url.clone(),
        cookie: req.cookie.clone(),
        request_headers: req.request_headers.clone(),
        response_headers: HashMap::new(),
        method: None,
        user_agent: req.user_agent.as_deref().map(sanitize_header_value),
        tab_url: None,
        referer: req.referer.as_deref().map(sanitize_header_value),
    };

    let downloader_state = item.get_downloader_state(req.output_path.clone());
    let record = PersistedDownload::from_request(&req, downloader_state);
    let id = record.id.clone();

    let status = match state.db.insert_download(&record) {
        Ok(()) => match start_download_record(record, Arc::clone(&state)).await {
            Ok(()) => "queued".to_string(),
            Err(e) => {
                let _ = state.db.update_status(&id, DownloadStatus::Failed, Some(&e));
                format!("error: {}", e)
            }
        },
        Err(e) => format!("error: {}", e),
    };

    Json(DownloadResponse { id, status })
}

async fn tab_update_handler(
    State(state): State<Arc<AppState>>,
    Json(data): Json<TabUpdateData>,
) -> Json<SyncConfig> {
    {
        let mut tracker = state.video_tracker.write().await;
        tracker.update_title_for_tab(&data.tab_url, &data.tab_title);
    }

    Json(sync_config(&state).await)
}

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
                "[vid] spawning UI for id=\"{}\"  url=\"{}\"  file=\"{}\"",
                item.id, item.url, item.text,
            );
            spawn_ui_for_item(item);
        }
        Err(err) => log::warn!("[vid] {}", err),
    }

    Json(sync_config(&state).await)
}

fn spawn_ui_for_item(item: VideoListItem) {
    let item_json = match serde_json::to_string(&item) {
        Ok(j) => j,
        Err(e) => {
            log::error!("[vid] failed to serialize item: {}", e);
            return;
        }
    };

    let ui_bin = find_ui_binary();
    let mut child = match std::process::Command::new(&ui_bin)
        .arg("--download-stdin")
        .env("RDM_UI_MODE", "download-stdin")
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                "[vid] failed to spawn rdm_ui at {:?}: {}. Make sure rdm_ui is built and available.",
                ui_bin, e
            );
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(item_json.as_bytes()) {
            log::error!("[vid] failed to write to rdm_ui stdin: {}", e);
        }
    }
}

fn find_ui_binary() -> PathBuf {
    let bin_name = if cfg!(windows) { "rdm_ui.exe" } else { "rdm_ui" };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(bin_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(sep) {
            let candidate = PathBuf::from(dir).join(bin_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    PathBuf::from(bin_name)
}

async fn start_download_record(record: PersistedDownload, state: Arc<AppState>) -> Result<(), String> {
    let mut downloader = if record.segments.is_empty() {
        HttpDownloader::new(record.downloader_state.clone(), state.connections)
    } else {
        HttpDownloader::from_persisted(
            record.downloader_state.clone(),
            state.connections,
            record.segments.clone(),
        )
    };

    let (sse_observer, progress_watch_rx) = SseProgressObserver::new();
    downloader.add_observer(Box::new(sse_observer));
    downloader
        .prepare()
        .await
        .map_err(|e| format!("failed to prepare download {}: {}", record.id, e))?;

    let runtime_state = downloader.current_state();
    let runtime_segments = downloader.current_segments().await;
    let controller = downloader
        .strategy_handle()
        .ok_or_else(|| format!("download {} missing strategy after prepare", record.id))?;
    state
        .db
        .update_runtime(&record.id, DownloadStatus::Running, &runtime_state, &runtime_segments, None)?;

    let downloader = Arc::new(TokioMutex::new(downloader));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let persist_rx = progress_watch_rx.clone();

    state.downloads.write().await.insert(
        record.id.clone(),
        ActiveDownload {
            downloader: Arc::clone(&downloader),
            controller,
            progress_rx: progress_watch_rx,
            stop_requested: Arc::clone(&stop_requested),
        },
    );

    spawn_progress_persist(record.id.clone(), state.db.clone(), persist_rx);
    spawn_download_runner(record.id, downloader, stop_requested, Arc::clone(&state));
    Ok(())
}

fn spawn_progress_persist(id: String, db: DownloadDatabase, mut rx: watch::Receiver<EnrichedSnapshot>) {
    tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let snapshot = rx.borrow_and_update().clone();
            if let Err(e) = db.update_progress(&id, &snapshot) {
                log::warn!("[db] failed to persist progress for {}: {}", id, e);
            }
            if snapshot.done {
                break;
            }
        }
    });
}

fn spawn_download_runner(
    id: String,
    downloader: Arc<TokioMutex<HttpDownloader>>,
    stop_requested: Arc<AtomicBool>,
    state: Arc<AppState>,
) {
    tokio::spawn(async move {
        let result = downloader.lock().await.run_prepared().await;
        let (status, last_error) = match &result {
            Ok(()) => (DownloadStatus::Completed, None),
            Err(DownloadError::Cancelled) if stop_requested.load(Ordering::Relaxed) => {
                (DownloadStatus::Stopped, None)
            }
            Err(DownloadError::Cancelled) => (
                DownloadStatus::Interrupted,
                Some("download interrupted".to_string()),
            ),
            Err(err) => (DownloadStatus::Failed, Some(err.to_string())),
        };

        let (runtime_state, runtime_segments) = {
            let guard = downloader.lock().await;
            (guard.current_state(), guard.current_segments().await)
        };

        if let Err(e) = state.db.update_runtime(
            &id,
            status,
            &runtime_state,
            &runtime_segments,
            last_error.as_deref(),
        ) {
            log::warn!("[db] failed to persist runtime for {}: {}", id, e);
        }
        if status == DownloadStatus::Completed {
            let completed_snapshot = EnrichedSnapshot {
                segments: Vec::new(),
                total_bytes_downloaded: runtime_state.file_size.max(0) as u64,
                total_bytes: runtime_state.file_size.max(0) as u64,
                speed: 0.0,
                eta_secs: 0.0,
                done: true,
            };
            let _ = state.db.update_progress(&id, &completed_snapshot);
        }

        state.downloads.write().await.remove(&id);
    });
}

async fn downloads_handler(State(state): State<Arc<AppState>>) -> Json<Vec<DownloadSummary>> {
    let mut downloads = match state.db.list_downloads() {
        Ok(rows) => rows,
        Err(e) => {
            log::warn!("[db] failed to list downloads: {}", e);
            Vec::new()
        }
    };

    let active = state.downloads.read().await;
    let summaries = downloads
        .iter_mut()
        .map(|record| {
            if let Some(active_dl) = active.get(&record.id) {
                let snapshot = active_dl.progress_rx.borrow().clone();
                record.status = DownloadStatus::Running;
                record.total_bytes_downloaded = snapshot.total_bytes_downloaded;
                record.total_bytes = snapshot.total_bytes;
                record.speed = snapshot.speed;
                record.eta_secs = snapshot.eta_secs;
                record.last_error = None;
                record.to_summary(true)
            } else {
                record.to_summary(false)
            }
        })
        .collect();

    Json(summaries)
}

async fn download_detail_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<DownloadSummary>, StatusCode> {
    let mut record = state
        .db
        .get_download(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(active_dl) = state.downloads.read().await.get(&id) {
        let snapshot = active_dl.progress_rx.borrow().clone();
        record.status = DownloadStatus::Running;
        record.total_bytes_downloaded = snapshot.total_bytes_downloaded;
        record.total_bytes = snapshot.total_bytes;
        record.speed = snapshot.speed;
        record.eta_secs = snapshot.eta_secs;
        record.last_error = None;
        Ok(Json(record.to_summary(true)))
    } else {
        Ok(Json(record.to_summary(false)))
    }
}

async fn status_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    match download_detail_handler(State(state), Path(id.clone())).await {
        Ok(Json(summary)) => serde_json::to_value(summary)
            .map(Json)
            .unwrap_or_else(|_| Json(serde_json::json!({ "id": id, "status": "error" }))),
        Err(StatusCode::NOT_FOUND) => Json(serde_json::json!({ "id": id, "status": "not_found" })),
        Err(_) => Json(serde_json::json!({ "id": id, "status": "error" })),
    }
}

async fn stop_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let downloader = {
        let mut found = None;
        let downloads = state.downloads.read().await;
        if let Some(dl) = downloads.get(&id) {
            dl.stop_requested.store(true, Ordering::Relaxed);
            found = Some(Arc::clone(&dl.controller));
        }
        found
    };

    let Some(downloader) = downloader else {
        return Json(serde_json::json!({ "id": id, "status": "not_found" }));
    };

    match downloader.stop().await {
        Ok(()) => {
            let _ = state.db.update_status(&id, DownloadStatus::Stopped, None);
            Json(serde_json::json!({ "id": id, "status": "stopped" }))
        }
        Err(e) => Json(serde_json::json!({
            "id": id,
            "status": "error",
            "detail": e.to_string()
        })),
    }
}

async fn resume_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    if state.downloads.read().await.contains_key(&id) {
        return Json(serde_json::json!({ "id": id, "status": "already_running" }));
    }

    let Some(record) = state.db.get_download(&id).ok().flatten() else {
        return Json(serde_json::json!({ "id": id, "status": "not_found" }));
    };

    let resumable = record.downloader_state.resumable && !record.segments.is_empty();
    if !resumable {
        return Json(serde_json::json!({
            "id": id,
            "status": "not_resumable",
            "detail": "This download does not have persisted resume data."
        }));
    }

    match start_download_record(record, Arc::clone(&state)).await {
        Ok(()) => Json(serde_json::json!({ "id": id, "status": "running" })),
        Err(e) => {
            let _ = state.db.update_status(&id, DownloadStatus::Failed, Some(&e));
            Json(serde_json::json!({ "id": id, "status": "error", "detail": e }))
        }
    }
}

async fn delete_entry_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    if state.downloads.read().await.contains_key(&id) {
        return Json(serde_json::json!({
            "id": id,
            "status": "error",
            "detail": "Stop the download before deleting it."
        }));
    }

    match state.db.delete_download(&id) {
        Ok(()) => Json(serde_json::json!({ "id": id, "status": "deleted_entry" })),
        Err(e) => Json(serde_json::json!({ "id": id, "status": "error", "detail": e })),
    }
}

async fn delete_with_files_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    if state.downloads.read().await.contains_key(&id) {
        return Json(serde_json::json!({
            "id": id,
            "status": "error",
            "detail": "Stop the download before deleting files."
        }));
    }

    let Some(record) = state.db.get_download(&id).ok().flatten() else {
        return Json(serde_json::json!({ "id": id, "status": "not_found" }));
    };

    if let Err(e) = delete_download_files(&record) {
        return Json(serde_json::json!({ "id": id, "status": "error", "detail": e }));
    }

    match state.db.delete_download(&id) {
        Ok(()) => Json(serde_json::json!({ "id": id, "status": "deleted_entry_and_files" })),
        Err(e) => Json(serde_json::json!({ "id": id, "status": "error", "detail": e })),
    }
}

fn delete_download_files(record: &PersistedDownload) -> Result<(), String> {
    let output_path = PathBuf::from(&record.output_path);
    if output_path.exists() {
        std::fs::remove_file(&output_path)
            .map_err(|e| format!("failed to delete {:?}: {}", output_path, e))?;
    }

    let temp_dir = PathBuf::from(&record.downloader_state.temp_dir);
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)
            .map_err(|e| format!("failed to delete temp dir {:?}: {}", temp_dir, e))?;
    }

    Ok(())
}

async fn progress_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let mut rx = {
        let downloads = state.downloads.read().await;
        let dl = downloads.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        dl.progress_rx.clone()
    };

    let stream = async_stream::stream! {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let snap = rx.borrow_and_update().clone();
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

async fn clear_handler(State(state): State<Arc<AppState>>) -> Json<SyncConfig> {
    {
        let mut tracker = state.video_tracker.write().await;
        tracker.clear();
    }
    Json(sync_config(&state).await)
}

async fn videos_handler(State(state): State<Arc<AppState>>) -> Json<Vec<VideoListItem>> {
    let tracker = state.video_tracker.read().await;
    Json(tracker.get_list())
}

async fn add_video_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(item): Json<VideoListItem>,
) -> Json<serde_json::Value> {
    let mut tracker = state.video_tracker.write().await;
    tracker.add_or_update(item);
    Json(serde_json::json!({ "status": "ok", "id": id }))
}

async fn remove_video_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let mut tracker = state.video_tracker.write().await;
    tracker.remove(&id);
    Json(serde_json::json!({ "status": "ok", "id": id }))
}

async fn echo_handler(State(_state): State<Arc<AppState>>, Path(msg): Path<String>) {
    log::info!("echo {}", msg);
}

fn default_db_path() -> PathBuf {
    dirs_next::data_local_dir()
        .or_else(dirs_next::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rdm")
        .join("downloads.sqlite3")
}

fn uuid_from_url(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn sanitize_header_value(s: &str) -> String {
    s.chars()
        .filter(|&c| c != '\r' && c != '\n' && c != '\0')
        .collect()
}

#[allow(dead_code)]
fn spawn_download(item: VideoListItem, state: Arc<AppState>) {
    let mime = if item.info.is_empty() { None } else { Some(item.info.as_str()) };
    let output_path = safe_output_path(&item.text, &item.url, mime);
    log::info!("[vid] output_path={:?}", output_path);

    let req = DownloadRequest {
        id: item.id.clone(),
        url: item.url.clone(),
        title: item.text.clone(),
        output_path: output_path.to_string_lossy().to_string(),
        cookie: item.cookie.clone(),
        request_headers: item.request_headers.clone(),
        user_agent: item.user_agent.clone(),
        referer: item.referer.clone(),
        info: item.info.clone(),
    };

    tokio::spawn(async move {
        let _ = download_handler(State(state), Json(req)).await;
    });
}
