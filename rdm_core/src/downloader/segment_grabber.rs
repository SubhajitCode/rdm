use std::path::PathBuf;

use futures::StreamExt;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::types::types::{DownloadError, ProbeResult, Segment, SegmentState};

/// Returns the exponential backoff delay in milliseconds for a given retry attempt.
/// Produces: attempt 1→200ms, 2→400ms, 3→800ms, capped at 32×100ms = 3200ms.
fn retry_backoff_ms(attempt: usize) -> u64 {
    100u64 * (1u64 << attempt.min(5))
}



/// Sends a probe request to determine file size, resumability, and metadata.
/// Uses `Range: bytes=0-0` to request only 1 byte, minimizing wasted bandwidth.
/// The file size is extracted from the `Content-Range` header.
pub async fn probe_url(
    client: &Client,
    url : &str,
) -> Result<ProbeResult, DownloadError> {
    let mut builder = client.get(url);
    builder = builder.header("Range", "bytes=0-0");
    let response = builder.send().await?;
    let resumable = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;

    // Content-Range (e.g. "bytes 0-0/1234567") is more reliable than Content-Length
    // when using Range: bytes=0-0.
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
        max_connections: 0,
    };

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
    temp_dir: PathBuf,
    cancel_token: CancellationToken,
    on_progress: impl Fn(u64),
    url:&str,
) -> Result<Segment, DownloadError> {
    let mut segment = segment;
    let mut retries = 0;
    const MAX_RETRIES: usize = 3;

    segment.state = SegmentState::Downloading;
    loop {
        if cancel_token.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }

        let mut builder = client.get(url);
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

                // Non-2xx responses (e.g. 503 Service Unavailable, 429 Too Many Requests)
                // are valid HTTP but indicate a server-side error. reqwest::send() succeeds
                // at the transport level, so these would otherwise fall through and write the
                // error response body (e.g. an HTML error page) into the segment temp file.
                // Treat them as retryable failures instead.
                if !status.is_success() {
                    log::warn!(
                        "[download_segment] segment={}: received non-success status={}, retrying (attempt {}/{})",
                        segment.id, status, retries + 1, MAX_RETRIES
                    );
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        segment.state = SegmentState::Failed;
                        return Err(DownloadError::MaxRetryExceeded);
                    }
                    // Exponential backoff: 200ms, 400ms, 800ms
                    let delay_ms = retry_backoff_ms(retries);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }

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

                // For non-resumable downloads (length == -1) accept everything the server sends.
                let remaining = if segment.length > 0 {
                    (segment.length - segment.downloaded) as u64
                } else {
                    u64::MAX
                };
                let mut bytes_written: u64 = 0;

                let mut stream = response.bytes_stream();
                let mut stream_error = false;

                while let Some(chunk_result) = stream.next().await {
                    if cancel_token.is_cancelled() {
                        let _ = writer.flush().await;
                        return Err(DownloadError::Cancelled);
                    }

                    match chunk_result {
                        Ok(chunk) => {
                            // Cap write to the remaining bytes this segment needs.
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

                            if segment.length > 0 && bytes_written >= remaining {
                                log::debug!(
                                    "[download_segment] segment={}: reached expected length {}, stopping stream",
                                    segment.id, segment.length
                                );
                                break;
                            }
                        }
                        Err(_e) => {
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
                    let delay_ms = retry_backoff_ms(retries);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }

                writer.flush().await.map_err(DownloadError::Disk)?;

                log::info!(
                    "[download_segment] segment={}: finished. downloaded={} bytes, expected_length={} bytes, match={}",
                    segment.id, segment.downloaded, segment.length,
                    if segment.length > 0 { segment.downloaded == segment.length } else { true }
                );

                // Size mismatch: the server delivered fewer (or more) bytes than the
                // segment required. Treat this as a retryable failure rather than
                // silently assembling a corrupt output file.
                if segment.length > 0 && segment.downloaded != segment.length {
                    log::warn!(
                        "[download_segment] segment={}: size mismatch! downloaded={} but expected={}. \
                         Treating as a transient error and retrying (attempt {}/{}).",
                        segment.id, segment.downloaded, segment.length, retries + 1, MAX_RETRIES
                    );
                    // Reset downloaded counter so the next attempt re-fetches the full segment.
                    segment.downloaded = 0;
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        segment.state = SegmentState::Failed;
                        return Err(DownloadError::MaxRetryExceeded);
                    }
                    // Exponential backoff: 200ms, 400ms, 800ms
                    let delay_ms = retry_backoff_ms(retries);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
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
                let delay_ms = retry_backoff_ms(retries);
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
    if let Some(name) = extract_filename_star(disposition) {
        return Some(name);
    }
    extract_filename_plain(disposition)
}

/// Extract `filename*=UTF-8''...` (RFC 5987 extended notation).
fn extract_filename_star(disposition: &str) -> Option<String> {
    let lower = disposition.to_lowercase();
    let key = "filename*=";
    let idx = lower.find(key)?;
    let rest = &disposition[idx + key.len()..];
    let rest = rest.split(';').next().unwrap_or(rest).trim();

    // Format: charset 'language' encoded-value — only UTF-8 is handled.
    let after_charset = if let Some(s) = rest.strip_prefix("UTF-8''").or_else(|| rest.strip_prefix("utf-8''")) {
        s
    } else {
        return None;
    };

    Some(percent_decode(after_charset))
}

/// Percent-decode a URL-encoded string (e.g. `My%20File.mp4` → `My File.mp4`).
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut pending: Vec<u8> = Vec::new();

    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                let hex = format!("{}{}", h1, h2);
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    pending.push(byte);
                    continue;
                }
            }
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
    let end = slice.find(';').unwrap_or(slice.len());
    let raw = slice[..end].trim().trim_matches('"');
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

pub(crate) async fn probe_segment(client: &Client, url: &String, segment: &Segment) -> Result<String, DownloadError> {
    let mut builder = client.head(url);

    if segment.length > 0 {
        // probe the exact byte range for this segment
        let start = segment.offset + segment.downloaded;
        let end = segment.offset + segment.length - 1;
        builder = builder.header("Range", format!("bytes={}-{}", start, end));
    }

    let response = builder.send().await?;
    // ensure 2xx; convert reqwest::Error into DownloadError via From
    if !response.status().is_success() {
        return Err(DownloadError::SegmentFailed("Probe request failed with status: ".to_string() + &response.status().to_string()));
    }
    log::info!(
        "[probe_segment] segment={}: probe response status={},offset={}, length={}",
        segment.id, response.status(),segment.offset , segment.length
    );
    Ok(response.status().to_string())
}
