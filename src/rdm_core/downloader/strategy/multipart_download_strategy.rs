use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::rdm_core::downloader::piece_grabber::{download_piece, probe_url};
use crate::rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use crate::rdm_core::types::types::{
    DownloadError, DownloaderState, HeaderData, Piece, ProgressEvent, SegmentState,
};

/// Default maximum number of concurrent download connections.
const MAX_CONNECTIONS: usize = 8;

/// Minimum piece size in bytes (256 KB). Pieces won't be split below this.
const MIN_PIECE_SIZE: i64 = 256 * 1024;

pub struct MultipartDownloadStrategy {
    state: Arc<RwLock<Option<DownloaderState>>>,
    pieces: Arc<RwLock<HashMap<String, Piece>>>,
    client: Arc<Client>,
    cancel_token: CancellationToken,
    progress_tx: mpsc::Sender<ProgressEvent>,
}

impl MultipartDownloadStrategy {
    pub fn new(
        url: String,
        _output_path: PathBuf,
        progress_tx: mpsc::Sender<ProgressEvent>,
    ) -> Self {
        let id = Uuid::new_v4().to_string();
        let temp_dir = std::env::temp_dir().join(&id);

        Self {
            state: Arc::new(RwLock::new(Some(DownloaderState {
                id,
                url,
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
            }))),
            pieces: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(Client::new()),
            cancel_token: CancellationToken::new(),
            progress_tx,
        }
    }

    /// Returns the temp directory path from the current state, if available.
    pub async fn temp_dir(&self) -> Option<String> {
        let state = self.state.read().await;
        state.as_ref().map(|s| s.temp_dir.clone())
    }

    /// Returns a reference to the internal state lock (for testing/inspection).
    pub fn state(&self) -> &Arc<RwLock<Option<DownloaderState>>> {
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
pub fn create_pieces(file_size: u64, max_connections: usize) -> Vec<Piece> {
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
    state: &Arc<RwLock<Option<DownloaderState>>>,
) -> Result<HeaderData, DownloadError> {
    let state_guard = state.read().await;
    let s = state_guard.as_ref().ok_or(DownloadError::InvalidState)?;
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

        // 3. Update state with probe results
        {
            let mut state = self.state.write().await;
            let s = state.as_mut().ok_or(DownloadError::InvalidState)?;
            s.file_size = probe.resource_size.map(|sz| sz as i64).unwrap_or(-1);
            s.url = probe.final_uri.clone(); // follow redirects
            s.last_modified = probe.last_modified.clone();
            s.resumable = probe.resumable;
            s.attachment_name = probe.attachment_name.clone();
            s.content_type = probe.content_type.clone();
        }

        // 4. Create temp directory
        {
            let state = self.state.read().await;
            let s = state.as_ref().ok_or(DownloadError::InvalidState)?;
            std::fs::create_dir_all(&s.temp_dir).map_err(DownloadError::Disk)?;
        }

        // 5. Create pieces based on probe results
        let new_pieces = if probe.resumable {
            if let Some(file_size) = probe.resource_size {
                create_pieces(file_size, MAX_CONNECTIONS)
            } else {
                // Resumable but unknown size — single piece, open-ended
                vec![Piece::new(Uuid::new_v4().to_string(), 0, -1)]
            }
        } else {
            // Non-resumable — single piece, download everything
            vec![Piece::new(Uuid::new_v4().to_string(), 0, -1)]
        };

        // 6. Store pieces
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
        let header_data = build_header_data(&self.state).await?;

        let temp_dir = {
            let state = self.state.read().await;
            let s = state.as_ref().ok_or(DownloadError::InvalidState)?;
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

        // Mark all pieces as Downloading
        {
            let mut pieces_guard = self.pieces.write().await;
            for piece in &pieces_to_download {
                if let Some(p) = pieces_guard.get_mut(&piece.id) {
                    p.state = SegmentState::Downloading;
                }
            }
        }

        // Spawn a tokio task for each piece — true concurrent downloads
        let mut handles = Vec::with_capacity(pieces_to_download.len());

        for piece in pieces_to_download {
            let client = Arc::clone(&self.client);
            let header_data = header_data.clone();
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

        // Wait for all tasks to complete and update piece states
        let mut first_error: Option<DownloadError> = None;

        for (piece_id, handle) in handles {
            match handle.await {
                Ok(Ok(updated_piece)) => {
                    // Piece downloaded successfully
                    let mut pieces_guard = self.pieces.write().await;
                    pieces_guard.insert(piece_id, updated_piece);
                }
                Ok(Err(e)) => {
                    // download_piece returned an error
                    let mut pieces_guard = self.pieces.write().await;
                    if let Some(p) = pieces_guard.get_mut(&piece_id) {
                        p.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
                Err(join_err) => {
                    // Task panicked or was aborted
                    let mut pieces_guard = self.pieces.write().await;
                    if let Some(p) = pieces_guard.get_mut(&piece_id) {
                        p.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error = Some(DownloadError::PieceFailed(join_err.to_string()));
                    }
                }
            }
        }

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
        let pieces = self.pieces.read().await;
        let state = self.state.read().await;

        let state = state.as_ref().ok_or(DownloadError::InvalidState)?;

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

        let temp_dir = state.temp_dir.clone();
        let output_file = state
            .attachment_name
            .clone()
            .unwrap_or_else(|| "download.bin".to_string());

        // Collect piece IDs in order (clone to move into spawn_blocking)
        let piece_ids: Vec<String> = sorted.iter().map(|p| p.id.clone()).collect();

        // File assembly is CPU/IO bound — run on a blocking thread
        tokio::task::spawn_blocking(move || {
            use std::fs::File;
            use std::io::{BufReader, BufWriter, Read};

            let mut output = BufWriter::new(File::create(&output_file)?);

            for piece_id in &piece_ids {
                let piece_path = PathBuf::from(&temp_dir).join(piece_id);
                let mut input = BufReader::new(File::open(&piece_path)?);
                let mut buf = [0u8; 64 * 1024]; // 64 KB copy buffer
                loop {
                    let n = input.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    output.write_all(&buf[..n])?;
                }
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
