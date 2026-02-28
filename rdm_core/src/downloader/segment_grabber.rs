use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use futures::StreamExt;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::types::types::{DownloadError, HeaderData, ProbeResult, Segment, SegmentState};

/// Applies common headers (custom headers, cookies, auth) to a request builder.
/// Skips the `Range` header — rdm sets its own Range per segment/probe, and a
/// stale browser-captured Range would create a duplicate causing the server
/// to return incorrect data.
fn apply_headers(
    mut builder: reqwest::RequestBuilder,
    header_data: &HeaderData,
    precomputed_auth: Option<&str>,
) -> reqwest::RequestBuilder {
    for (key, values) in &header_data.headers {
        if key.eq_ignore_ascii_case("range") {
            continue;
        }
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

/// Downloads a single segment of a file.
///
/// For resumable downloads, sends `Range: bytes={start}-{end}`.
/// For non-resumable downloads (segment.length == -1), sends no Range header
/// and downloads the entire response body.
///
/// Uses async I/O (tokio::fs) with a 256 KB write buffer to avoid blocking
/// the tokio runtime. Retries with exponential backoff on network errors.
pub async fn download_segment(
    segment: Segment,
    client: &Client,
    header_data: &Arc<HeaderData>,
    temp_dir: PathBuf,
    cancel_token: CancellationToken,
    on_progress: impl Fn(u64),
) -> Result<Segment, DownloadError> {
    let mut segment = segment;
    let mut retries = 0;
    const MAX_RETRIES: usize = 3;

    segment.state = SegmentState::Downloading;

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
        if segment.length > 0 {
            let start = segment.offset + segment.downloaded;
            let end = segment.offset + segment.length - 1;
            log::info!(
                "[download_segment] segment={}: requesting Range: bytes={}-{} (offset={}, length={}, already_downloaded={})",
                segment.id, start, end, segment.offset, segment.length, segment.downloaded
            );
            builder = builder.header("Range", format!("bytes={}-{}", start, end));
        } else {
            log::info!(
                "[download_segment] segment={}: no Range header (non-resumable, length={})",
                segment.id, segment.length
            );
        }

        match builder.send().await {
            Ok(response) => {
                let status = response.status();
                let content_length = response.content_length();
                log::info!(
                    "[download_segment] segment={}: response status={}, content_length={:?}, expected_segment_length={}",
                    segment.id, status, content_length, segment.length
                );

                // BUG DETECTION: If we sent a Range request but got 200 (not 206),
                // the server ignored our Range header and is sending the ENTIRE file.
                // Each of the N segments will download the full file, resulting in Nx file size.
                if segment.length > 0 && status == reqwest::StatusCode::OK {
                    log::error!(
                        "[download_segment] BUG: segment={}: sent Range request but server responded with 200 OK instead of 206 Partial Content! \
                         The server is sending the ENTIRE file body ({:?} bytes) instead of just the requested range. \
                         This segment expected only {} bytes. With {} connections, the final file will be {}x too large.",
                        segment.id,
                        content_length,
                        segment.length,
                        8, // MAX_CONNECTIONS
                        8  // MAX_CONNECTIONS
                    );
                }

                // Open temp file with async I/O + 256 KB write buffer
                let file_path = temp_dir.join(&segment.id);
                let file = if segment.downloaded > 0 {
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

                // How many bytes this segment still needs. For non-resumable
                // downloads (length == -1) we accept everything the server sends.
                let remaining = if segment.length > 0 {
                    (segment.length - segment.downloaded) as u64
                } else {
                    u64::MAX
                };
                let mut bytes_written: u64 = 0;

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
                            // Cap the write to the remaining bytes this segment needs.
                            // Servers may ignore the Range header and send the full
                            // file body even when responding with 206; without this
                            // guard every segment would contain the entire file and the
                            // assembled output would be N× too large.
                            let to_write = if segment.length > 0 {
                                let left = remaining - bytes_written;
                                let usable = (chunk.len() as u64).min(left);
                                &chunk[..usable as usize]
                            } else {
                                &chunk[..]
                            };

                            if to_write.is_empty() {
                                // Already received all the bytes we need — stop early.
                                log::debug!(
                                    "[download_segment] segment={}: received all {} expected bytes, stopping stream",
                                    segment.id, segment.length
                                );
                                break;
                            }

                            writer
                                .write_all(to_write)
                                .await
                                .map_err(DownloadError::Disk)?;
                            let written_len = to_write.len() as u64;
                            bytes_written += written_len;
                            segment.downloaded += written_len as i64;
                            on_progress(written_len);

                            // If we have exactly enough, stop reading.
                            if segment.length > 0 && bytes_written >= remaining {
                                log::debug!(
                                    "[download_segment] segment={}: reached expected length {}, stopping stream",
                                    segment.id, segment.length
                                );
                                break;
                            }
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
                        segment.state = SegmentState::Failed;
                        return Err(DownloadError::MaxRetryExceeded);
                    }
                    // Exponential backoff: 100ms, 200ms, 400ms
                    let delay_ms = 100u64 * (1u64 << retries.min(5));
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }

                writer.flush().await.map_err(DownloadError::Disk)?;

                log::info!(
                    "[download_segment] segment={}: finished. downloaded={} bytes, expected_length={} bytes, match={}",
                    segment.id, segment.downloaded, segment.length,
                    if segment.length > 0 { segment.downloaded == segment.length } else { true }
                );

                // BUG DETECTION: downloaded more bytes than the segment should contain
                if segment.length > 0 && segment.downloaded != segment.length {
                    log::error!(
                        "[download_segment] BUG: segment={}: size mismatch! downloaded={} but expected={}. \
                         The server likely ignored the Range header and sent the full file. \
                         This will cause the assembled output to be larger than the original file.",
                        segment.id, segment.downloaded, segment.length
                    );
                }

                segment.state = SegmentState::Finished;
                return Ok(segment);
            }
            Err(_e) => {
                retries += 1;
                if retries >= MAX_RETRIES {
                    segment.state = SegmentState::Failed;
                    return Err(DownloadError::MaxRetryExceeded);
                }
                // Exponential backoff: 100ms, 200ms, 400ms
                let delay_ms = 100u64 * (1u64 << retries.min(5));
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// Extract the filename from a `Content-Disposition` header value.
///
/// Handles both the plain `filename=` form and the RFC 5987 `filename*=`
/// extended form (e.g. `filename*=UTF-8''My%20File.mp4`).  The RFC 5987
/// form takes priority when both are present.
pub fn extract_filename(disposition: &str) -> Option<String> {
    // RFC 5987: filename*=charset'language'encoded-value (preferred)
    if let Some(name) = extract_filename_star(disposition) {
        return Some(name);
    }

    // Plain filename="..." or filename=...
    extract_filename_plain(disposition)
}

/// Extract `filename*=UTF-8''...` (RFC 5987 extended notation).
fn extract_filename_star(disposition: &str) -> Option<String> {
    // Case-insensitive search for "filename*="
    let lower = disposition.to_lowercase();
    let key = "filename*=";
    let idx = lower.find(key)?;
    let rest = &disposition[idx + key.len()..];
    // Strip optional surrounding whitespace / quotes
    let rest = rest.split(';').next().unwrap_or(rest).trim();

    // Format: charset'language'encoded-value
    // We only handle UTF-8 (the overwhelmingly common case).
    let after_charset = if let Some(s) = rest.strip_prefix("UTF-8''").or_else(|| rest.strip_prefix("utf-8''")) {
        s
    } else {
        // Unknown charset — fall through to plain filename=
        return None;
    };

    // Percent-decode the value.
    Some(percent_decode(after_charset))
}

/// Percent-decode a URL-encoded string (e.g. `My%20File.mp4` → `My File.mp4`).
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    // Collect bytes for multi-byte UTF-8 sequences
    let mut pending: Vec<u8> = Vec::new();

    while let Some(c) = chars.next() {
        if c == '%' {
            // Try to read two hex digits
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                let hex = format!("{}{}", h1, h2);
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    pending.push(byte);
                    continue;
                }
            }
            // Not valid hex — flush pending and emit literally
            flush_pending(&mut pending, &mut out);
            out.push('%');
            if let Some(h1) = h1 {
                out.push(h1);
            }
            if let Some(h2) = h2 {
                out.push(h2);
            }
        } else {
            flush_pending(&mut pending, &mut out);
            out.push(c);
        }
    }
    flush_pending(&mut pending, &mut out);
    out
}

fn flush_pending(pending: &mut Vec<u8>, out: &mut String) {
    if pending.is_empty() {
        return;
    }
    if let Ok(s) = std::str::from_utf8(pending) {
        out.push_str(s);
    } else {
        // Replace invalid UTF-8 sequences with replacement character
        out.push('\u{FFFD}');
    }
    pending.clear();
}

/// Extract a plain `filename=` value (with or without quotes).
fn extract_filename_plain(disposition: &str) -> Option<String> {
    let lower = disposition.to_lowercase();
    let key = "filename=";
    let idx = lower.find(key)?;
    let start = idx + key.len();
    let slice = &disposition[start..];
    // Terminate at `;` (next parameter boundary)
    let end = slice.find(';').unwrap_or(slice.len());
    let raw = slice[..end].trim().trim_matches('"');
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}
