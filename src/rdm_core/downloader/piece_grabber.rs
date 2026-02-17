use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use futures::StreamExt;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::rdm_core::types::types::{DownloadError, HeaderData, Piece, ProbeResult, SegmentState};

/// Applies common headers (custom headers, cookies, auth) to a request builder.
fn apply_headers(
    mut builder: reqwest::RequestBuilder,
    header_data: &HeaderData,
    precomputed_auth: Option<&str>,
) -> reqwest::RequestBuilder {
    for (key, values) in &header_data.headers {
        for value in values {
            builder = builder.header(key, value);
        }
    }
    if let Some(cookies) = &header_data.cookies {
        builder = builder.header("Cookie", cookies);
    }
    if let Some(auth_value) = precomputed_auth {
        builder = builder.header("Authorization", auth_value);
    }
    builder
}

/// Pre-computes the Basic auth header value, if authentication is configured.
fn precompute_auth(header_data: &HeaderData) -> Option<String> {
    header_data.authentication.as_ref().map(|auth| {
        let credentials = format!("{}:{}", auth.username, auth.password);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&credentials);
        format!("Basic {}", encoded)
    })
}

/// Sends a probe request to determine file size, resumability, and metadata.
/// Uses `Range: bytes=0-0` to request only 1 byte, minimizing wasted bandwidth.
/// The file size is extracted from the `Content-Range` header.
pub async fn probe_url(
    client: &Client,
    header_data: &HeaderData,
) -> Result<ProbeResult, DownloadError> {
    let auth_header = precompute_auth(header_data);
    let builder = client.get(&header_data.url);
    let mut builder = apply_headers(builder, header_data, auth_header.as_deref());

    // Request only 1 byte to test resumability and get total size
    builder = builder.header("Range", "bytes=0-0");

    let response = builder.send().await?;

    let resumable = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;

    // Parse file size from Content-Range header (e.g. "bytes 0-0/1234567")
    // This is more reliable than Content-Length when using Range: bytes=0-0
    let resource_size = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.rsplit('/').next())
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| response.content_length());

    let probe = ProbeResult {
        resumable,
        resource_size,
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

    // Drop response — only 1 byte of body data, minimal waste
    drop(response);

    Ok(probe)
}

/// Downloads a single piece (segment) of a file.
///
/// For resumable downloads, sends `Range: bytes={start}-{end}`.
/// For non-resumable downloads (piece.length == -1), sends no Range header
/// and downloads the entire response body.
///
/// Uses async I/O (tokio::fs) with a 256 KB write buffer to avoid blocking
/// the tokio runtime. Retries with exponential backoff on network errors.
pub async fn download_piece(
    piece: Piece,
    client: &Client,
    header_data: &Arc<HeaderData>,
    temp_dir: PathBuf,
    cancel_token: CancellationToken,
    on_progress: impl Fn(u64),
) -> Result<Piece, DownloadError> {
    let mut piece = piece;
    let mut retries = 0;
    const MAX_RETRIES: usize = 3;

    piece.state = SegmentState::Downloading;

    // Pre-compute auth header once (avoids format! + base64 on every retry)
    let auth_header = precompute_auth(header_data);

    loop {
        if cancel_token.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }

        // Build request with shared helper
        let builder = client.get(&header_data.url);
        let mut builder = apply_headers(builder, header_data, auth_header.as_deref());

        // Add Range header for resumable downloads
        if piece.length > 0 {
            let start = piece.offset + piece.downloaded;
            let end = piece.offset + piece.length - 1;
            builder = builder.header("Range", format!("bytes={}-{}", start, end));
        }

        match builder.send().await {
            Ok(response) => {
                // Open temp file with async I/O + 256 KB write buffer
                let file_path = temp_dir.join(&piece.id);
                let file = if piece.downloaded > 0 {
                    tokio::fs::OpenOptions::new()
                        .append(true)
                        .open(&file_path)
                        .await
                        .map_err(DownloadError::Disk)?
                } else {
                    tokio::fs::File::create(&file_path)
                        .await
                        .map_err(DownloadError::Disk)?
                };
                let mut writer = tokio::io::BufWriter::with_capacity(256 * 1024, file);

                // Stream the response body chunk by chunk
                let mut stream = response.bytes_stream();
                let mut stream_error = false;

                while let Some(chunk_result) = stream.next().await {
                    if cancel_token.is_cancelled() {
                        let _ = writer.flush().await;
                        return Err(DownloadError::Cancelled);
                    }

                    match chunk_result {
                        Ok(chunk) => {
                            writer
                                .write_all(&chunk)
                                .await
                                .map_err(DownloadError::Disk)?;
                            let chunk_len = chunk.len() as u64;
                            piece.downloaded += chunk_len as i64;
                            on_progress(chunk_len);
                        }
                        Err(_e) => {
                            // Network error mid-stream — flush what we have, then retry
                            let _ = writer.flush().await;
                            stream_error = true;
                            break;
                        }
                    }
                }

                if stream_error {
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        piece.state = SegmentState::Failed;
                        return Err(DownloadError::MaxRetryExceeded);
                    }
                    // Exponential backoff: 100ms, 200ms, 400ms
                    let delay_ms = 100u64 * (1u64 << retries.min(5));
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }

                writer.flush().await.map_err(DownloadError::Disk)?;
                piece.state = SegmentState::Finished;
                return Ok(piece);
            }
            Err(_e) => {
                retries += 1;
                if retries >= MAX_RETRIES {
                    piece.state = SegmentState::Failed;
                    return Err(DownloadError::MaxRetryExceeded);
                }
                // Exponential backoff: 100ms, 200ms, 400ms
                let delay_ms = 100u64 * (1u64 << retries.min(5));
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
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
