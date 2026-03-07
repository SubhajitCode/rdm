pub mod segment_grabber;
pub mod http_downloader;
pub mod strategy;
mod util;

use std::sync::{Arc, RwLock};

use reqwest::Client;
use tokio::sync::mpsc;

use crate::types::types::{DownloadError, DownloaderState, ProgressEvent, ProbeResult};
use crate::downloader::segment_grabber::probe_url;

/// Returns a closure that, when called with a `bytes_delta`, sends a `ProgressEvent`
/// to the provided `tx` channel. Silently drops the event if the channel is full or absent.
///
/// Using this helper deduplicates the identical progress-closure boilerplate that
/// both `MultipartDownloadStrategy` and `OnePartDownloadStrategy` previously inlined.
pub(crate) fn make_progress_sender(
    tx: Option<mpsc::Sender<Result<ProgressEvent, String>>>,
    segment_id: String,
    offset: u64,
    total_bytes: Option<u64>,
) -> impl Fn(u64) {
    move |bytes_delta| {
        if let Some(ref tx) = tx {
            let _ = tx.try_send(Ok(ProgressEvent {
                segment_id: segment_id.clone(),
                offset,
                bytes_delta,
                total_bytes,
            }));
        }
    }
}

/// Probes the URL if the state hasn't been pre-populated (e.g. via `from_probe()`).
///
/// Detection heuristic: if `file_size == -1` AND both `content_type` and
/// `attachment_name` are `None`, the state was created cold and needs a probe.
/// Both `MultipartDownloadStrategy` and `OnePartDownloadStrategy` use this check
/// to avoid duplicating the probe condition.
///
/// Returns the `ProbeResult` if a probe was performed, or `None` if it was skipped.
pub(crate) async fn probe_if_needed(
    state: &Arc<RwLock<DownloaderState>>,
    client: &Client,
) -> Result<Option<ProbeResult>, DownloadError> {
    let needs_probe = {
        let s = state.read().unwrap();
        s.file_size == -1 && s.content_type.is_none() && s.attachment_name.is_none()
    };

    if !needs_probe {
        return Ok(None);
    }

    let url = state.read().unwrap().url.clone();
    let probe = probe_url(client, &url).await?;

    {
        let mut s = state.write().unwrap();
        s.resumable = probe.resumable;
        s.file_size = probe.resource_size.map(|sz| sz as i64).unwrap_or(-1);
        s.url = probe.final_uri.clone();
        s.attachment_name = probe.attachment_name.clone();
        s.content_type = probe.content_type.clone();
        s.last_modified = probe.last_modified.clone();
    }

    Ok(Some(probe))
}
