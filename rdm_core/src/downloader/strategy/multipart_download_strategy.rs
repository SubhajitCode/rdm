use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::downloader::piece_grabber::{download_piece, probe_url};
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::types::types::{AuthenticationInfo, DownloadError, DownloaderState, HeaderData, Piece, ProgressEvent, ProxyInfo, SegmentState};

/// Default maximum number of concurrent download connections.
const MAX_CONNECTIONS: usize = 8;

/// Minimum piece size in bytes (256 KB). Pieces won't be split below this.
const MIN_PIECE_SIZE: i64 = 256 * 1024;

pub struct MultipartDownloadStrategy {
    state: Arc<RwLock<DownloaderState>>,
    pieces: Arc<RwLock<HashMap<String, Piece>>>,
    client: Arc<Client>,
    cancel_token: CancellationToken,
    progress_tx: mpsc::Sender<ProgressEvent>,
}
pub struct MultipartDownloadStrategyBuilder{
    strategy: MultipartDownloadStrategy,
    event_receiver: mpsc::Receiver<ProgressEvent>,
}

impl MultipartDownloadStrategy {
    pub fn new(
        url: String,
        output_path: PathBuf,
        progress_tx: mpsc::Sender<ProgressEvent>,
    ) -> Self {
        let id = Uuid::new_v4().to_string();
        let temp_dir = std::env::temp_dir().join(&id);
        let output_path_str = output_path.to_string_lossy().to_string();

        Self {
            state: Arc::new(RwLock::new(DownloaderState {
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
            pieces: Arc::new(RwLock::new(HashMap::new())),
            // Tuned HTTP client: connection pool, timeouts, TCP optimizations
            client: Arc::new(
                Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .pool_max_idle_per_host(MAX_CONNECTIONS) // match concurrency
                    .tcp_nodelay(true)
                    .build()
                    .expect("failed to build HTTP client"),
            ),
            cancel_token: CancellationToken::new(),
            progress_tx,
        }
    }

    pub fn builder(url:String,path:PathBuf) -> MultipartDownloadStrategyBuilder {
        MultipartDownloadStrategyBuilder::new(url,path)
    }

    /// Returns the temp directory path from the current state, if available.
    pub async fn temp_dir(&self) -> String {
        let state = self.state.read().await;
        state.temp_dir.clone()
    }

    /// Returns a reference to the internal state lock (for testing/inspection).
    pub fn state(&self) -> &Arc<RwLock<DownloaderState>> {
        &self.state
    }

    /// Returns a reference to the internal pieces lock (for testing/inspection).
    pub fn pieces(&self) -> &Arc<RwLock<HashMap<String, Piece>>> {
        &self.pieces
    }

    /// Returns a reference to the cancellation token.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }
}

/// Creates download pieces using XDM-style dynamic halving.
///
/// Starts with a single piece covering the entire file, then repeatedly
/// splits the largest piece in half until we reach `max_connections` pieces
/// or every piece is at the minimum size.
fn create_pieces(file_size: u64, max_connections: usize) -> Vec<Piece> {
    // Start with one piece covering the whole file
    let mut pieces = vec![Piece::new(
        Uuid::new_v4().to_string(),
        0,
        file_size as i64,
    )];

    // Repeatedly halve the largest piece
    while pieces.len() < max_connections {
        // Find the piece with the most bytes
        let max_idx = pieces
            .iter()
            .enumerate()
            .max_by_key(|(_, p)| p.length)
            .map(|(i, _)| i)
            .unwrap();

        let piece = &pieces[max_idx];

        // Don't split if it would produce pieces below minimum size
        if piece.length < MIN_PIECE_SIZE * 2 {
            break;
        }

        let half = piece.length / 2;
        let new_offset = piece.offset + half;
        let new_length = piece.length - half;

        // Shrink the original piece
        pieces[max_idx].length = half;

        // Create the new piece for the second half
        pieces.push(Piece::new(
            Uuid::new_v4().to_string(),
            new_offset,
            new_length,
        ));
    }

    pieces
}

/// Extracts HeaderData from the current DownloaderState.
/// Acquires the read lock once and copies all needed fields.
async fn build_header_data(
    state: &Arc<RwLock<DownloaderState>>,
) -> Result<HeaderData, DownloadError> {
    let s = state.read().await;
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
    /// Probes the URL, determines file size and resumability, creates temp
    /// directory, and splits the file into download pieces.
    async fn preprocess(&self) -> Result<(), DownloadError> {
        // 1. Build HeaderData from current state
        let header_data = build_header_data(&self.state).await?;

        // 2. Probe the URL
        let probe = probe_url(&self.client, &header_data).await?;

        // 3. Extract Copy fields before moving probe
        let resumable = probe.resumable;
        let resource_size = probe.resource_size;

        // 4. Update state with probe results (move fields, don't clone)
        let temp_dir_path = {
            let mut s = self.state.write().await;
            s.file_size = resource_size.map(|sz| sz as i64).unwrap_or(-1);
            s.url = probe.final_uri;           // move
            s.last_modified = probe.last_modified;   // move
            s.resumable = resumable;
            s.attachment_name = probe.attachment_name;   // move
            s.content_type = probe.content_type;   // move
            s.temp_dir.clone()
        };

        // 5. Create temp directory (async, non-blocking)
        tokio::fs::create_dir_all(&temp_dir_path)
            .await
            .map_err(DownloadError::Disk)?;

        // 6. Create pieces based on probe results
        let new_pieces = if resumable {
            if let Some(file_size) = resource_size {
                create_pieces(file_size, MAX_CONNECTIONS)
            } else {
                // Resumable but unknown size — single piece, open-ended
                vec![Piece::new(Uuid::new_v4().to_string(), 0, -1)]
            }
        } else {
            // Non-resumable — single piece, download everything
            vec![Piece::new(Uuid::new_v4().to_string(), 0, -1)]
        };

        // 7. Store pieces
        {
            let mut pieces = self.pieces.write().await;
            pieces.clear();
            for piece in new_pieces {
                pieces.insert(piece.id.clone(), piece);
            }
        }

        Ok(())
    }

    /// Downloads all pieces concurrently. Each piece is downloaded in its own
    /// tokio task. Waits for all tasks to complete and propagates errors.
    async fn download(&self) -> Result<(), DownloadError> {
        // Wrap HeaderData in Arc — shared across all piece tasks without cloning
        let header_data = Arc::new(build_header_data(&self.state).await?);

        let temp_dir = {
            let s = self.state.read().await;
            PathBuf::from(&s.temp_dir)
        };

        // Collect all pieces that need downloading
        let pieces_to_download: Vec<Piece> = {
            let pieces_guard = self.pieces.read().await;
            pieces_guard
                .values()
                .filter(|p| p.state == SegmentState::NotStarted)
                .cloned()
                .collect()
        };

        if pieces_to_download.is_empty() {
            return Ok(());
        }

        // No need to mark pieces as Downloading here — download_piece() does it
        // at piece_grabber.rs:90, and the cloned copies in the HashMap are never
        // read during the download phase.

        // Spawn a tokio task for each piece — true concurrent downloads
        let mut handles = Vec::with_capacity(pieces_to_download.len());

        for piece in pieces_to_download {
            let client = Arc::clone(&self.client);
            let header_data = Arc::clone(&header_data); // cheap Arc clone
            let temp_dir = temp_dir.clone();
            let cancel_token = self.cancel_token.clone();
            let progress_tx = self.progress_tx.clone();
            let piece_id_for_progress = piece.id.clone();
            let piece_id_for_handle = piece.id.clone();

            let handle = tokio::spawn(async move {
                download_piece(
                    piece,
                    &client,
                    &header_data,
                    temp_dir,
                    cancel_token,
                    |bytes_downloaded| {
                        let _ = progress_tx.try_send(ProgressEvent {
                            piece_id: piece_id_for_progress.clone(),
                            bytes_downloaded,
                            total_bytes: None,
                            speed: 0,
                            progress: 0,
                        });
                    },
                )
                .await
            });

            handles.push((piece_id_for_handle, handle));
        }

        // Wait for ALL tasks to complete, then update pieces in a single lock
        let results: Vec<_> = futures::future::join_all(
            handles.into_iter().map(|(id, handle)| async move {
                (id, handle.await)
            }),
        )
        .await;

        let mut pieces_guard = self.pieces.write().await;
        let mut first_error: Option<DownloadError> = None;

        for (piece_id, result) in results {
            match result {
                Ok(Ok(updated_piece)) => {
                    pieces_guard.insert(piece_id, updated_piece);
                }
                Ok(Err(e)) => {
                    if let Some(p) = pieces_guard.get_mut(&piece_id) {
                        p.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
                Err(join_err) => {
                    if let Some(p) = pieces_guard.get_mut(&piece_id) {
                        p.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error = Some(DownloadError::PieceFailed(join_err.to_string()));
                    }
                }
            }
        }

        drop(pieces_guard);

        if let Some(e) = first_error {
            return Err(e);
        }

        Ok(())
    }

    async fn pause(&self) -> Result<(), DownloadError> {
        // Cancel the token to stop all in-flight downloads.
        // On resume, a new token would be created and incomplete pieces restarted.
        self.cancel_token.cancel();
        Ok(())
    }

    async fn stop(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    /// Assembles all downloaded pieces into the final output file.
    /// Sorts pieces by offset and concatenates their temp files.
    async fn postprocess(&self) -> Result<(), DownloadError> {
        // Extract all needed data under locks, then drop them before I/O
        let (piece_ids, temp_dir, output_file) = {
            let pieces = self.pieces.read().await;
            let state = self.state.read().await;

            // Verify all pieces are finished
            for piece in pieces.values() {
                if piece.state != SegmentState::Finished {
                    return Err(DownloadError::PieceFailed(format!(
                        "piece {} is in state {:?}, expected Finished",
                        piece.id, piece.state
                    )));
                }
            }

            // Sort pieces by offset
            let mut sorted: Vec<_> = pieces.values().collect();
            sorted.sort_by_key(|p| p.offset);

            let piece_ids: Vec<String> = sorted.iter().map(|p| p.id.clone()).collect();
            let temp_dir = state.temp_dir.clone();
            let output_file = state
                .output_path
                .clone()
                .or_else(|| state.attachment_name.clone())
                .unwrap_or_else(|| "download.bin".to_string());

            (piece_ids, temp_dir, output_file)
        }; // locks dropped here — not held during I/O

        // File assembly is CPU/IO bound — run on a blocking thread
        tokio::task::spawn_blocking(move || {
            use std::fs::File;
            use std::io::Write;

            let mut output = File::create(&output_file)?;

            for piece_id in &piece_ids {
                let piece_path = PathBuf::from(&temp_dir).join(piece_id);
                let mut input = File::open(&piece_path)?;
                // Use std::io::copy — on Linux it uses copy_file_range(2) for
                // zero-copy kernel-side transfer. On macOS, it uses an optimized
                // internal buffer. Much better than manual 64KB buffer loops.
                std::io::copy(&mut input, &mut output)?;
            }

            output.flush()?;

            // Clean up temp files
            for piece_id in &piece_ids {
                let piece_path = PathBuf::from(&temp_dir).join(piece_id);
                let _ = std::fs::remove_file(piece_path);
            }
            let _ = std::fs::remove_dir(&temp_dir);

            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|e| DownloadError::PieceFailed(e.to_string()))?
        .map_err(DownloadError::Disk)?;

        Ok(())
    }
}
impl MultipartDownloadStrategyBuilder {
    pub fn new(url: String, path: PathBuf) -> Self {
        let (progress_tx, progress_rx) = mpsc::channel(256);
        let strategy = MultipartDownloadStrategy::new(url, path, progress_tx);
        Self {
            strategy,
            event_receiver: progress_rx,
        }
    }

    pub fn with_cookies(self, cookies: String) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.cookies = Some(cookies);
        }
        self
    }

    pub fn with_headers(self, headers: HashMap<String, Vec<String>>) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
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
            let mut state = self.strategy.state.blocking_write();
            let key = key.into();
            let value = value.into();
            state.headers.entry(key).or_insert_with(Vec::new).push(value);
        }
        self
    }

    pub fn with_authentication(self, auth: AuthenticationInfo) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.authentication = Some(auth);
        }
        self
    }

    pub fn with_proxy(self, proxy: ProxyInfo) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.proxy = Some(proxy);
        }
        self
    }

    pub fn with_convert_to_mp3(self, convert: bool) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.convert_to_mp3 = convert;
        }
        self
    }

    pub fn with_last_modified(self, last_modified: String) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.last_modified = Some(last_modified);
        }
        self
    }

    pub fn with_attachment_name(mut self, name: String) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.attachment_name = Some(name);
        }
        self
    }

    pub fn with_content_type(mut self, content_type: String) -> Self {
        {
            let mut state = self.strategy.state.blocking_write();
            state.content_type = Some(content_type);
        }
        self
    }

    pub fn build(self) -> (MultipartDownloadStrategy, mpsc::Receiver<ProgressEvent>) {
        (self.strategy, self.event_receiver)
    }
}
