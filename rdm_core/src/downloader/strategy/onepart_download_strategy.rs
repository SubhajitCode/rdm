use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::downloader::segment_grabber::{download_segment, probe_url};
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::downloader::util::ensure_extension;
use crate::types::types::{
    DownloadError, DownloaderState, HeaderData, ProbeResult, ProgressEvent, Segment,
};

pub struct OnePartDownloadStrategy {
    state: Arc<StdRwLock<DownloaderState>>,
    client: Arc<Client>,
    cancel_token: CancellationToken,
    /// `None` while no progress consumer is attached (events are silently dropped).
    progress_tx: StdMutex<Option<mpsc::Sender<Result<ProgressEvent, String>>>>,
    /// Set after `download()` completes — holds the temp file name (= segment UUID).
    downloaded_segment_id: StdMutex<Option<String>>,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl OnePartDownloadStrategy {
    /// Construct from an already-built `DownloaderState`.
    pub fn from_state(state: DownloaderState) -> Self {
        let client = state.create_client();
        Self {
            state: Arc::new(StdRwLock::new(state)),
            client: Arc::new(client),
            cancel_token: CancellationToken::new(),
            progress_tx: StdMutex::new(None),
            downloaded_segment_id: StdMutex::new(None),
        }
    }

    /// Construct from a pre-fetched `ProbeResult` (avoids a second HTTP round-trip).
    /// State metadata is applied immediately so `preprocess()` only creates the temp dir.
    pub fn from_probe(mut state: DownloaderState, probe: ProbeResult) -> Self {
        state.file_size = probe.resource_size.map(|sz| sz as i64).unwrap_or(-1);
        state.url = probe.final_uri;
        state.last_modified = probe.last_modified;
        state.resumable = false;
        state.attachment_name = probe.attachment_name;
        state.content_type = probe.content_type;
        Self::from_state(state)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_header_data(state: &Arc<StdRwLock<DownloaderState>>) -> HeaderData {
    let s = state.read().unwrap();
    HeaderData {
        url: s.url.clone(),
        headers: s.headers.clone(),
        cookies: s.cookies.clone(),
        authentication: s.authentication.clone(),
        proxy: s.proxy.clone(),
    }
}

// ---------------------------------------------------------------------------
// DownloadStrategy impl
// ---------------------------------------------------------------------------

#[async_trait]
impl DownloadStrategy for OnePartDownloadStrategy {
    fn set_progress_tx(&self, tx: mpsc::Sender<Result<ProgressEvent, String>>) {
        *self.progress_tx.lock().unwrap() = Some(tx);
    }

    fn clear_progress_tx(&self) {
        *self.progress_tx.lock().unwrap() = None;
    }

    /// Creates the temp directory.
    /// If `from_probe` was used, metadata is already populated and no HTTP probe
    /// is issued.  Otherwise, probes the URL to populate state first.
    async fn preprocess(&self) -> Result<(), DownloadError> {
        let already_probed = {
            let s = self.state.read().unwrap();
            // file_size -1 + resumable false == uninitialised (default from DownloaderState::new)
            s.file_size != -1 || s.content_type.is_some() || s.attachment_name.is_some()
        };

        if !already_probed {
            let header_data = build_header_data(&self.state);
            let probe = probe_url(&self.client, &header_data).await?;
            let mut s = self.state.write().unwrap();
            s.file_size = probe.resource_size.map(|sz| sz as i64).unwrap_or(-1);
            s.url = probe.final_uri;
            s.last_modified = probe.last_modified;
            s.resumable = false;
            s.attachment_name = probe.attachment_name;
            s.content_type = probe.content_type;
        }

        let temp_dir_path = self.state.read().unwrap().temp_dir.clone();
        tokio::fs::create_dir_all(&temp_dir_path)
            .await
            .map_err(DownloadError::Disk)?;

        log::info!("[onepart::preprocess] temp_dir={}", temp_dir_path);
        Ok(())
    }

    /// Downloads the entire resource as a single segment into the temp directory.
    async fn download(&self) -> Result<(), DownloadError> {
        let progress_tx = self.progress_tx.lock().unwrap().clone();

        let temp_dir = {
            let s = self.state.read().unwrap();
            PathBuf::from(&s.temp_dir)
        };

        let header_data = Arc::new(build_header_data(&self.state));
        let segment_id = Uuid::new_v4().to_string();
        // length == -1 → download without a Range header (full body)
        let segment = Segment::new(segment_id.clone(), 0, -1);

        let client = Arc::clone(&self.client);
        let cancel_token = self.cancel_token.clone();
        let segment_id_for_progress = segment_id.clone();

        let result = download_segment(
            segment,
            &client,
            &header_data,
            temp_dir.clone(),
            cancel_token,
            |bytes_delta| {
                if let Some(tx) = &progress_tx {
                    let _ = tx.try_send(Ok(ProgressEvent {
                        segment_id: segment_id_for_progress.clone(),
                        bytes_delta,
                        total_bytes: None,
                    }));
                }
            },
        )
        .await;

        match result {
            Ok(finished_segment) => {
                *self.downloaded_segment_id.lock().unwrap() = Some(finished_segment.id);
                Ok(())
            }
            Err(e) => {
                if let Some(tx) = &progress_tx {
                    let _ = tx.try_send(Err(e.to_string()));
                }
                Err(e)
            }
        }
    }

    async fn pause(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    async fn stop(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    /// Moves the single downloaded temp file to the final output path.
    async fn postprocess(&self) -> Result<(), DownloadError> {
        let segment_id = self
            .downloaded_segment_id
            .lock()
            .unwrap()
            .clone()
            .ok_or(DownloadError::InvalidState)?;

        let (temp_dir, output_file) = {
            let s = self.state.read().unwrap();

            let base_output = s
                .output_path
                .clone()
                .or_else(|| s.attachment_name.clone())
                .unwrap_or_else(|| "download.bin".to_string());

            let output_file = ensure_extension(
                base_output,
                s.attachment_name.as_deref(),
                s.content_type.as_deref(),
            );

            (s.temp_dir.clone(), output_file)
        };

        let segment_path = PathBuf::from(&temp_dir).join(&segment_id);

        tokio::task::spawn_blocking(move || {
            log::info!(
                "[onepart::postprocess] moving {:?} -> {}",
                segment_path,
                output_file
            );

            // Prefer rename (atomic, zero-copy on same filesystem).
            // Fall back to copy+delete if crossing filesystem boundaries.
            if std::fs::rename(&segment_path, &output_file).is_err() {
                std::fs::copy(&segment_path, &output_file)?;
                let _ = std::fs::remove_file(&segment_path);
            }

            let _ = std::fs::remove_dir(&temp_dir);

            log::info!("[onepart::postprocess] complete: output={}", output_file);
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|e| DownloadError::SegmentFailed(e.to_string()))?
        .map_err(DownloadError::Disk)?;

        Ok(())
    }
}
