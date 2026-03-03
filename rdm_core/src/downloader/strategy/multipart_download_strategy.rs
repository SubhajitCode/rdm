use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::downloader::segment_grabber::{download_segment};
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::types::types::{
    AuthenticationInfo, DownloadError, DownloaderState, ProgressEvent, ProbeResult,
    ProxyInfo, Segment, SegmentState,
};

const MAX_CONNECTIONS: usize = 8;

/// Minimum segment size in bytes (256 KB). Segments won't be split below this.
const MIN_SEGMENT_SIZE: i64 = 256 * 1024;

pub struct MultipartDownloadStrategy {
    state: Arc<StdRwLock<DownloaderState>>,
    segments: Arc<RwLock<HashMap<String, Segment>>>,
    client: Arc<Client>,
    cancel_token: CancellationToken,
    /// `None` while no progress consumer is attached (events are silently dropped).
    progress_tx: StdMutex<Option<mpsc::Sender<Result<ProgressEvent, String>>>>,
    connections: usize,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl MultipartDownloadStrategy {
    /// Internal constructor — wraps an already-built `DownloaderState`.
    pub fn from_state(state: DownloaderState, connections: usize) -> Self {
        let client = state.create_client();
        Self {
            state: Arc::new(StdRwLock::new(state)),
            segments: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(client),
            cancel_token: CancellationToken::new(),
            progress_tx: StdMutex::new(None),
            connections,
        }
    }

    /// Convenience constructor for simple cases (no cookies/headers/proxy).
    pub fn new(url: String, output_path: PathBuf) -> Self {
        let state = DownloaderState::new(url, output_path);
        Self::from_state(state, MAX_CONNECTIONS)
    }

    /// Entry point for the fluent builder API.
    pub fn builder(url: String, path: PathBuf) -> MultipartDownloadStrategyBuilder {
        MultipartDownloadStrategyBuilder::new(url, path)
    }

    /// Construct from a pre-fetched `ProbeResult` (avoids a second HTTP round-trip).
    /// The probe's metadata (file_size, resumable, attachment_name, etc.) is applied
    /// to the state before returning, so `preprocess()` becomes a no-op temp-dir creation.
    pub fn from_probe(mut state: DownloaderState, probe: ProbeResult, connections: usize) -> Self {
        state.file_size = probe.resource_size.map(|sz| sz as i64).unwrap_or(-1);
        state.url = probe.final_uri;
        state.last_modified = probe.last_modified;
        state.resumable = probe.resumable;
        state.attachment_name = probe.attachment_name;
        state.content_type = probe.content_type;
        Self::from_state(state, connections)
    }

    pub async fn temp_dir(&self) -> String {
        self.state.read().unwrap().temp_dir.clone()
    }

    pub fn state(&self) -> &Arc<StdRwLock<DownloaderState>> {
        &self.state
    }

    pub fn segments(&self) -> &Arc<RwLock<HashMap<String, Segment>>> {
        &self.segments
    }

    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }
}

// ---------------------------------------------------------------------------
// Builder — plain-struct, no locking during construction
// ---------------------------------------------------------------------------

pub struct MultipartDownloadStrategyBuilder {
    url: String,
    output_path: PathBuf,
    cookies: Option<String>,
    headers: HashMap<String, Vec<String>>,
    authentication: Option<AuthenticationInfo>,
    proxy: Option<ProxyInfo>,
    convert_to_mp3: bool,
    last_modified: Option<String>,
    attachment_name: Option<String>,
    content_type: Option<String>,
    connections: usize,
}

impl MultipartDownloadStrategyBuilder {
    pub fn new(url: String, path: PathBuf) -> Self {
        Self {
            url,
            output_path: path,
            cookies: None,
            headers: HashMap::new(),
            authentication: None,
            proxy: None,
            convert_to_mp3: false,
            last_modified: None,
            attachment_name: None,
            content_type: None,
            connections: MAX_CONNECTIONS,
        }
    }

    pub fn with_cookies(mut self, cookies: String) -> Self {
        self.cookies = Some(cookies);
        self
    }

    pub fn with_headers(mut self, headers: HashMap<String, Vec<String>>) -> Self {
        self.headers = headers;
        self
    }

    pub fn add_header<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.headers.insert(key.into(), vec![value.into()]);
        self
    }

    pub fn with_authentication(mut self, auth: AuthenticationInfo) -> Self {
        self.authentication = Some(auth);
        self
    }

    pub fn with_proxy(mut self, proxy: ProxyInfo) -> Self {
        self.proxy = Some(proxy);
        self
    }

    pub fn with_convert_to_mp3(mut self, convert: bool) -> Self {
        self.convert_to_mp3 = convert;
        self
    }

    pub fn with_last_modified(mut self, last_modified: String) -> Self {
        self.last_modified = Some(last_modified);
        self
    }

    pub fn with_attachment_name(mut self, name: String) -> Self {
        self.attachment_name = Some(name);
        self
    }

    pub fn with_content_type(mut self, content_type: String) -> Self {
        self.content_type = Some(content_type);
        self
    }

    pub fn with_connection_size(mut self, connections: usize) -> Self {
        self.connections = connections;
        self
    }

    pub fn build(self) -> MultipartDownloadStrategy {
        let mut state = DownloaderState::new(self.url, self.output_path);
        state.cookies = self.cookies;
        state.headers = self.headers;
        state.authentication = self.authentication;
        state.proxy = self.proxy;
        state.convert_to_mp3 = self.convert_to_mp3;
        state.last_modified = self.last_modified;
        state.attachment_name = self.attachment_name;
        state.content_type = self.content_type;
        MultipartDownloadStrategy::from_state(state, self.connections)
    }
}

// ---------------------------------------------------------------------------
// Segment creation
// ---------------------------------------------------------------------------

/// Creates download segments using XDM-style dynamic halving.
///
/// Starts with a single segment covering the entire file, then repeatedly
/// splits the largest segment in half until we reach `max_connections` segments
/// or every segment is at the minimum size.
fn create_segments(file_size: u64, max_connections: usize) -> Vec<Segment> {
    log::info!(
        "[create_segments] file_size={}, max_connections={}",
        file_size,
        max_connections
    );

    let mut segments = vec![Segment::new(
        Uuid::new_v4().to_string(),
        0,
        file_size as i64,
    )];

    while segments.len() < max_connections {
        let max_idx = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, s)| s.length)
            .map(|(i, _)| i)
            .unwrap();

        let segment = &segments[max_idx];

        if segment.length < MIN_SEGMENT_SIZE * 2 {
            log::debug!(
                "[create_segments] stopping split: largest segment length={} < MIN_SEGMENT_SIZE*2={}",
                segment.length,
                MIN_SEGMENT_SIZE * 2
            );
            break;
        }

        let half = segment.length / 2;
        let new_offset = segment.offset + half;
        let new_length = segment.length - half;

        log::debug!(
            "[create_segments] splitting segment[{}]: offset={}, length={} -> half={}, new_offset={}, new_length={}",
            max_idx, segment.offset, segment.length, half, new_offset, new_length
        );

        segments[max_idx].length = half;

        segments.push(Segment::new(
            Uuid::new_v4().to_string(),
            new_offset,
            new_length,
        ));
    }

    let total: i64 = segments.iter().map(|s| s.length).sum();
    log::info!(
        "[create_segments] created {} segments, total_bytes={}, file_size={}",
        segments.len(),
        total,
        file_size
    );
    for (i, s) in segments.iter().enumerate() {
        log::debug!(
            "[create_segments]   segment[{}]: offset={}, length={}, end={}",
            i,
            s.offset,
            s.length,
            s.offset + s.length - 1
        );
    }

    segments
}

// ---------------------------------------------------------------------------
// DownloadStrategy impl
// ---------------------------------------------------------------------------

#[async_trait]
impl DownloadStrategy for MultipartDownloadStrategy {
    fn set_progress_tx(&self, tx: mpsc::Sender<Result<ProgressEvent, String>>) {
        *self.progress_tx.lock().unwrap() = Some(tx);
    }

    fn clear_progress_tx(&self) {
        *self.progress_tx.lock().unwrap() = None;
    }

    async fn preprocess(&self) -> Result<(), DownloadError> {

        //already probed as part of MultipartDownloadStrategy::from_probe()?
        let (resumable, resource_size) = {
            let s = self.state.read().unwrap();
            let size = if s.file_size > 0 { Some(s.file_size as u64) } else { None };
            (s.resumable, size)
        };

        let temp_dir_path = self.state.read().unwrap().temp_dir.clone();
        tokio::fs::create_dir_all(&temp_dir_path)
            .await
            .map_err(DownloadError::Disk)?;

        let new_segments = if resumable {
            if let Some(file_size) = resource_size {
                log::info!(
                    "[preprocess] resumable=true, file_size={}, creating multipart segments with max_connections={}",
                    file_size, self.connections
                );
                //different segmentation strategy could be applied here TODO.
                create_segments(file_size, self.connections)
            } else {
                log::info!("[preprocess] resumable=true but file_size unknown, using single segment");
                vec![Segment::new(Uuid::new_v4().to_string(), 0, -1)]
            }
        } else {
            log::info!("[preprocess] resumable=false, using single segment (full download)");
            vec![Segment::new(Uuid::new_v4().to_string(), 0, -1)]
        };

        {
            let mut segments = self.segments.write().await;
            segments.clear();
            for segment in new_segments {
                segments.insert(segment.id.clone(), segment);
            }
        }

        Ok(())
    }

    async fn download(&self) -> Result<(), DownloadError> {
        let progress_tx: Option<mpsc::Sender<Result<ProgressEvent, String>>> =
            self.progress_tx.lock().unwrap().clone();

        // let header_data = Arc::new(build_header_data(&self.state)?);

        let temp_dir = {
            let s = self.state.read().unwrap();
            PathBuf::from(&s.temp_dir)
        };

        let segments_to_download: Vec<Segment> = {
            let segments_guard = self.segments.read().await;
            segments_guard
                .values()
                .filter(|s| s.state == SegmentState::NotStarted)
                .cloned()
                .collect()
        };

        if segments_to_download.is_empty() {
            return Ok(());
        }

        // download_segment() marks segments as Downloading at segment_grabber.rs:90;
        // the cloned copies in the HashMap are not read during the download phase.
        let mut handles = Vec::with_capacity(segments_to_download.len());

        for segment in segments_to_download {
            let url = {
                let s = self.state.read().unwrap();
                s.url.clone()
            };
            let client = Arc::clone(&self.client);
            let temp_dir = temp_dir.clone();
            let cancel_token = self.cancel_token.clone();
            let segment_tx = progress_tx.clone();
            let segment_id_for_progress = segment.id.clone();
            let segment_id_for_handle = segment.id.clone();
            let segment_total_bytes = if segment.length > 0 {
                Some(segment.length as u64)
            } else {
                None
            };

            let handle = tokio::spawn(async move {
                download_segment(
                    segment,
                    &client,
                    temp_dir,
                    cancel_token,
                    |bytes_delta| {
                        if let Some(tx) = &segment_tx {
                            let _ = tx.try_send(Ok(ProgressEvent {
                                segment_id: segment_id_for_progress.clone(),
                                bytes_delta,
                                total_bytes: segment_total_bytes,
                            }));
                        }
                    },
                    url.as_str()
                )
                .await
            });

            handles.push((segment_id_for_handle, handle));
        }

        let results: Vec<_> = futures::future::join_all(
            handles.into_iter().map(|(id, handle)| async move { (id, handle.await) }),
        )
        .await;

        let mut segments_guard = self.segments.write().await;
        let mut first_error: Option<DownloadError> = None;

        for (segment_id, result) in results {
            match result {
                Ok(Ok(updated_segment)) => {
                    segments_guard.insert(segment_id, updated_segment);
                }
                Ok(Err(e)) => {
                    if let Some(s) = segments_guard.get_mut(&segment_id) {
                        s.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
                Err(join_err) => {
                    if let Some(s) = segments_guard.get_mut(&segment_id) {
                        s.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error =
                            Some(DownloadError::SegmentFailed(join_err.to_string()));
                    }
                }
            }
        }

        drop(segments_guard);

        if let Some(e) = first_error {
            if let Some(tx) = &progress_tx {
                let _ = tx.try_send(Err(e.to_string()));
            }
            return Err(e);
        }

        Ok(())
    }

    async fn pause(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    async fn stop(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    /// Assembles all downloaded segments into the final output file.
    /// Sorts segments by offset and concatenates their temp files.
    async fn postprocess(&self) -> Result<(), DownloadError> {
        let (segment_ids, temp_dir, output_file) = {
            let segments = self.segments.read().await;
            let state = self.state.read().unwrap();

            for segment in segments.values() {
                if segment.state != SegmentState::Finished {
                    return Err(DownloadError::SegmentFailed(format!(
                        "segment {} is in state {:?}, expected Finished",
                        segment.id, segment.state
                    )));
                }
            }

            let mut sorted: Vec<_> = segments.values().collect();
            sorted.sort_by_key(|s| s.offset);

            let segment_ids: Vec<String> = sorted.iter().map(|s| s.id.clone()).collect();
            let temp_dir = state.temp_dir.clone();

            // Resolve output path: pre-computed output_path → attachment_name → "download.bin".
            let base_output = state
                .output_path
                .clone()
                .or_else(|| state.attachment_name.clone())
                .unwrap_or_else(|| "download.bin".to_string());

            // If the path has no extension, try to derive one from attachment_name or MIME type.
            let output_file = crate::downloader::util::ensure_extension(
                base_output,
                state.attachment_name.as_deref(),
                state.content_type.as_deref(),
            );

            (segment_ids, temp_dir, output_file)
        };

        tokio::task::spawn_blocking(move || {
            use std::fs::File;
            use std::io::Write;

            let mut output = File::create(&output_file)?;
            let mut total_assembled: u64 = 0;

            for segment_id in &segment_ids {
                let segment_path = PathBuf::from(&temp_dir).join(segment_id);
                let segment_file_size = std::fs::metadata(&segment_path)?.len();
                log::info!(
                    "[postprocess] assembling segment={}: file_size={} bytes",
                    segment_id,
                    segment_file_size
                );
                total_assembled += segment_file_size;

                let mut input = File::open(&segment_path)?;
                std::io::copy(&mut input, &mut output)?;
            }

            output.flush()?;

            log::info!(
                "[postprocess] assembly complete: total_assembled={} bytes across {} segments, output={}",
                total_assembled,
                segment_ids.len(),
                output_file
            );

            for segment_id in &segment_ids {
                let segment_path = PathBuf::from(&temp_dir).join(segment_id);
                let _ = std::fs::remove_file(segment_path);
            }
            let _ = std::fs::remove_dir(&temp_dir);

            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|e| DownloadError::SegmentFailed(e.to_string()))?
        .map_err(DownloadError::Disk)?;

        Ok(())
    }
}
