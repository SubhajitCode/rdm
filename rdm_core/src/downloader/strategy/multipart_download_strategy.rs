use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::downloader::segment_grabber::{download_segment, probe_url};
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::types::types::{AuthenticationInfo, DownloadError, DownloaderState, HeaderData, Segment, ProgressEvent, ProxyInfo, SegmentState};

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
pub struct MultipartDownloadStrategyBuilder {
    strategy: MultipartDownloadStrategy,
}

impl MultipartDownloadStrategy {

    pub fn new(url: String, output_path: PathBuf) -> Self {
        let id = Uuid::new_v4().to_string();
        let temp_dir = std::env::temp_dir().join(&id);
        let output_path_str = output_path.to_string_lossy().to_string();

        Self {
            state: Arc::new(StdRwLock::new(DownloaderState {
                id,
                url,
                output_path: Some(output_path_str),
                temp_dir: temp_dir.to_string_lossy().to_string(),
                file_size: -1,
                headers: HashMap::new(),
                cookies: None,
                authentication: None,
                proxy: None,
                convert_to_mp3: false,
                last_modified: None,
                resumable: false,
                attachment_name: None,
                content_type: None,
            })),
            segments: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(
                Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .pool_max_idle_per_host(MAX_CONNECTIONS)
                    .tcp_nodelay(true)
                    .no_gzip()
                    .no_deflate()
                    .no_brotli()
                    .build()
                    .expect("failed to build HTTP client"),
            ),
            cancel_token: CancellationToken::new(),
            progress_tx: StdMutex::new(None),
            connections: MAX_CONNECTIONS,
        }
    }
    pub fn from_state(state: DownloaderState,connections:usize) -> Self {
        let client = state.get_client().clone();
        Self {
            state: Arc::new(StdRwLock::new(DownloaderState {
                ..state
            })),
            segments: Arc::new(RwLock::new(HashMap::new())),
            client: Arc:: new(client),
            cancel_token: CancellationToken::new(),
            progress_tx: StdMutex::new(None),
            connections,
        }
    }

    pub fn builder(url:String,path:PathBuf) -> MultipartDownloadStrategyBuilder {
        MultipartDownloadStrategyBuilder::new(url,path)
    }

    pub async fn temp_dir(&self) -> String {
        let state = self.state.read().unwrap();
        state.temp_dir.clone()
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
            i, s.offset, s.length, s.offset + s.length - 1
        );
    }

    segments
}

fn build_header_data(
    state: &Arc<StdRwLock<DownloaderState>>,
) -> Result<HeaderData, DownloadError> {
    let s = state.read().unwrap();
    Ok(HeaderData {
        url: s.url.clone(),
        headers: s.headers.clone(),
        cookies: s.cookies.clone(),
        authentication: s.authentication.clone(),
        proxy: s.proxy.clone(),
    })
}

#[async_trait]
impl DownloadStrategy for MultipartDownloadStrategy {
    fn set_progress_tx(&self, tx: mpsc::Sender<Result<ProgressEvent, String>>) {
        *self.progress_tx.lock().unwrap() = Some(tx);
    }

    fn clear_progress_tx(&self) {
        *self.progress_tx.lock().unwrap() = None;
    }

    async fn preprocess(&self) -> Result<(), DownloadError> {
        let header_data = build_header_data(&self.state)?;
        let probe = probe_url(&self.client, &header_data).await?;

        let resumable = probe.resumable;
        let resource_size = probe.resource_size;

        let temp_dir_path = {
            let mut s = self.state.write().unwrap();
            s.file_size = resource_size.map(|sz| sz as i64).unwrap_or(-1);
            s.url = probe.final_uri;
            s.last_modified = probe.last_modified;
            s.resumable = resumable;
            s.attachment_name = probe.attachment_name;
            s.content_type = probe.content_type;
            s.temp_dir.clone()
        };

        tokio::fs::create_dir_all(&temp_dir_path)
            .await
            .map_err(DownloadError::Disk)?;

        let new_segments = if resumable {
            if let Some(file_size) = resource_size {
                log::info!(
                    "[preprocess] resumable=true, file_size={}, creating multipart segments with max_connections={}",
                    file_size, self.connections
                );
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

        let header_data = Arc::new(build_header_data(&self.state)?);

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
            let client = Arc::clone(&self.client);
            let header_data = Arc::clone(&header_data);
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
                    &header_data,
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
                )
                .await
            });

            handles.push((segment_id_for_handle, handle));
        }

        let results: Vec<_> = futures::future::join_all(
            handles.into_iter().map(|(id, handle)| async move {
                (id, handle.await)
            }),
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
                        first_error = Some(DownloadError::SegmentFailed(join_err.to_string()));
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
            let output_file = ensure_extension(
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
                    segment_id, segment_file_size
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

// ---------------------------------------------------------------------------
// Extension helpers
// ---------------------------------------------------------------------------

/// If `path` already has a file extension, return it unchanged.
/// Otherwise try to derive an extension from `attachment_name` (Content-
/// Disposition) or `content_type` (MIME type) and append it.
fn ensure_extension(
    path: String,
    attachment_name: Option<&str>,
    content_type: Option<&str>,
) -> String {
    let pb = PathBuf::from(&path);
    if pb.extension().is_some() {
        return path;
    }

    let ext = attachment_name
        .and_then(|n| PathBuf::from(n).extension().map(|e| e.to_string_lossy().into_owned()))
        .or_else(|| ext_from_mime(content_type));

    match ext {
        Some(e) if !e.is_empty() => format!("{}.{}", path, e.to_lowercase()),
        _ => path,
    }
}

fn ext_from_mime(content_type: Option<&str>) -> Option<String> {
    let mime = content_type?
        .split(';')
        .next()?
        .trim()
        .to_lowercase();

    let ext = match mime.as_str() {
        "video/mp4" | "video/x-m4v"                        => "mp4",
        "video/x-matroska"                                  => "mkv",
        "video/webm"                                        => "webm",
        "video/x-msvideo"                                   => "avi",
        "video/quicktime"                                   => "mov",
        "video/x-ms-wmv"                                    => "wmv",
        "video/3gpp"                                        => "3gp",
        "video/x-flv"                                       => "flv",
        "video/mpeg"                                        => "mpg",
        "audio/mpeg"                                        => "mp3",
        "audio/flac"                                        => "flac",
        "audio/ogg"                                         => "ogg",
        "audio/wav" | "audio/x-wav"                        => "wav",
        "audio/aac"                                         => "aac",
        "audio/x-m4a" | "audio/mp4"                        => "m4a",
        "audio/opus"                                        => "opus",
        "application/zip"                                   => "zip",
        "application/x-tar"                                 => "tar",
        "application/gzip" | "application/x-gzip"          => "gz",
        "application/x-bzip2"                               => "bz2",
        "application/x-7z-compressed"                       => "7z",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/pdf"                                   => "pdf",
        "application/x-msdownload"                          => "exe",
        "application/x-ms-installer" | "application/x-msi" => "msi",
        "application/vnd.debian.binary-package"             => "deb",
        "application/x-rpm"                                 => "rpm",
        "application/x-apple-diskimage"                     => "dmg",
        _ => return None,
    };
    Some(ext.to_string())
}

impl MultipartDownloadStrategyBuilder {
    pub fn new(url: String, path: PathBuf) -> Self {
        Self {
            strategy: MultipartDownloadStrategy::new(url, path),
        }
    }

    pub fn from_state()-> Self {
        Self {
            strategy: MultipartDownloadStrategy::new("".to_string(), PathBuf::from("")),
        }
    }

    pub fn with_cookies(self, cookies: String) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.cookies = Some(cookies);
        }
        self
    }

    pub fn with_headers(self, headers: HashMap<String, Vec<String>>) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.headers = headers;
        }
        self
    }

    pub fn add_header<K, V>(self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        {
            let mut state = self.strategy.state.write().unwrap();
            let key = key.into();
            let value = value.into();
            // insert() replaces any existing value for the key, preventing duplicates
            // when the browser-captured request headers already contain the same key.
            state.headers.insert(key, vec![value]);
        }
        self
    }

    pub fn with_authentication(self, auth: AuthenticationInfo) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.authentication = Some(auth);
        }
        self
    }

    pub fn with_proxy(self, proxy: ProxyInfo) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.proxy = Some(proxy);
        }
        self
    }

    pub fn with_convert_to_mp3(self, convert: bool) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.convert_to_mp3 = convert;
        }
        self
    }

    pub fn with_last_modified(self, last_modified: String) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.last_modified = Some(last_modified);
        }
        self
    }

    pub fn with_attachment_name(self, name: String) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.attachment_name = Some(name);
        }
        self
    }

    pub fn with_content_type(self, content_type: String) -> Self {
        {
            let mut state = self.strategy.state.write().unwrap();
            state.content_type = Some(content_type);
        }
        self
    }
    
    pub fn with_connection_size(mut self, connections: usize) -> Self {
        {
            self.strategy.connections= connections;
        }
        self
    }

    pub fn build(self) -> MultipartDownloadStrategy {
        self.strategy
    }
}
