use std::io::Write;
use std::path::PathBuf;

use base64::Engine;
use futures::StreamExt;
use reqwest::Client;
use tokio_util::sync::CancellationToken;

use crate::rdm_core::types::types::{DownloadError, HeaderData, Piece, ProbeResult, SegmentState};

/// Sends a probe request to determine file size, resumability, and metadata.
/// Does NOT download any data — the response body is dropped.
pub async fn probe_url(
    client: &Client,
    header_data: &HeaderData,
) -> Result<ProbeResult, DownloadError> {
    let mut builder = client.get(&header_data.url);

    // Add custom headers
    for (key, values) in &header_data.headers {
        for value in values {
            builder = builder.header(key, value);
        }
    }

    // Add cookies
    if let Some(cookies) = &header_data.cookies {
        builder = builder.header("Cookie", cookies);
    }

    // Add authentication
    if let Some(auth) = &header_data.authentication {
        let credentials = format!("{}:{}", auth.username, auth.password);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&credentials);
        builder = builder.header("Authorization", format!("Basic {}", encoded));
    }

    // Send Range header to test resumability
    builder = builder.header("Range", "bytes=0-");

    let response = builder.send().await?;

    let probe = ProbeResult {
        resumable: response.status() == reqwest::StatusCode::PARTIAL_CONTENT,
        resource_size: response.content_length(),
        final_uri: response.url().to_string(),
        attachment_name: response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .and_then(extract_filename),
        content_type: response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        last_modified: response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    };

    // Drop response body — we only needed the headers
    drop(response);

    Ok(probe)
}

/// Downloads a single piece (segment) of a file.
///
/// For resumable downloads, sends `Range: bytes={start}-{end}`.
/// For non-resumable downloads (piece.length == -1), sends no Range header
/// and downloads the entire response body.
///
/// Downloads in a streaming fashion, writing chunks as they arrive.
/// Retries up to MAX_RETRIES times on network errors.
pub async fn download_piece(
    piece: Piece,
    client: &Client,
    header_data: &HeaderData,
    temp_dir: PathBuf,
    cancel_token: CancellationToken,
    on_progress: impl Fn(u64),
) -> Result<Piece, DownloadError> {
    let mut piece = piece;
    let mut retries = 0;
    const MAX_RETRIES: usize = 3;

    piece.state = SegmentState::Downloading;

    loop {
        if cancel_token.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }

        // Build request
        let mut builder = client.get(&header_data.url);

        // Add custom headers
        for (key, values) in &header_data.headers {
            for value in values {
                builder = builder.header(key, value);
            }
        }

        // Add cookies
        if let Some(cookies) = &header_data.cookies {
            builder = builder.header("Cookie", cookies);
        }

        // Add authentication
        if let Some(auth) = &header_data.authentication {
            let credentials = format!("{}:{}", auth.username, auth.password);
            let encoded = base64::engine::general_purpose::STANDARD.encode(&credentials);
            builder = builder.header("Authorization", format!("Basic {}", encoded));
        }

        // Add Range header for resumable downloads
        if piece.length > 0 {
            let start = piece.offset + piece.downloaded;
            let end = piece.offset + piece.length - 1;
            builder = builder.header("Range", format!("bytes={}-{}", start, end));
        }
        // For non-resumable (length == -1), no Range header — download everything

        match builder.send().await {
            Ok(response) => {
                // Open temp file (append if resuming a partially downloaded piece)
                let file_path = temp_dir.join(&piece.id);
                let mut file = if piece.downloaded > 0 {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&file_path)
                        .map_err(DownloadError::Disk)?
                } else {
                    std::fs::File::create(&file_path).map_err(DownloadError::Disk)?
                };

                // Stream the response body chunk by chunk
                let mut stream = response.bytes_stream();
                while let Some(chunk_result) = stream.next().await {
                    if cancel_token.is_cancelled() {
                        return Err(DownloadError::Cancelled);
                    }

                    match chunk_result {
                        Ok(chunk) => {
                            file.write_all(&chunk).map_err(DownloadError::Disk)?;
                            let chunk_len = chunk.len() as u64;
                            piece.downloaded += chunk_len as i64;
                            on_progress(chunk_len);
                        }
                        Err(_e) => {
                            // Network error mid-stream — retry
                            retries += 1;
                            if retries >= MAX_RETRIES {
                                piece.state = SegmentState::Failed;
                                return Err(DownloadError::MaxRetryExceeded);
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue; // This breaks the inner loop, outer loop retries
                        }
                    }
                }

                file.flush().map_err(DownloadError::Disk)?;
                piece.state = SegmentState::Finished;
                return Ok(piece);
            }
            Err(_e) => {
                retries += 1;
                if retries >= MAX_RETRIES {
                    piece.state = SegmentState::Failed;
                    return Err(DownloadError::MaxRetryExceeded);
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

pub fn extract_filename(disposition: &str) -> Option<String> {
    if let Some(idx) = disposition.find("filename=") {
        let start = idx + 9;
        let end = disposition[start..]
            .find(';')
            .unwrap_or(disposition.len() - start);
        Some(disposition[start..start + end].trim_matches('"').to_string())
    } else {
        None
    }
}
