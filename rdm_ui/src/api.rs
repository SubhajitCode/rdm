use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Shared types (mirror rdm_server's types for HTTP communication)
// ---------------------------------------------------------------------------

/// A detected streaming video item received from the server via CLI args.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoItem {
    pub id: String,
    pub text: String,
    pub info: String,
    #[serde(rename = "tabId", default)]
    pub tab_id: String,
    pub url: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "responseHeaders")]
    pub response_headers: HashMap<String, serde_json::Value>,
    pub method: Option<String>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    #[serde(rename = "tabUrl")]
    pub tab_url: Option<String>,
    pub referer: Option<String>,
}

/// Request payload for POST /download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub id: String,
    pub url: String,
    pub title: String,
    #[serde(rename = "outputPath")]
    pub output_path: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default, rename = "requestHeaders")]
    pub request_headers: HashMap<String, serde_json::Value>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    #[serde(default)]
    pub info: String,
}

/// Response from POST /download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResponse {
    pub id: String,
    pub status: String,
}

/// A progress snapshot received via SSE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
    pub done: bool,
}

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

const SERVER_BASE: &str = "http://127.0.0.1:8597";

/// Trigger a download by calling POST /download on rdmd.
/// Returns the download ID on success.
pub async fn trigger_download(req: &DownloadRequest) -> Result<DownloadResponse, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/download", SERVER_BASE))
        .json(req)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Server returned status {}", resp.status()));
    }

    resp.json::<DownloadResponse>()
        .await
        .map_err(|e| format!("Parse error: {}", e))
}

/// Cancel an active download by calling POST /cancel/{id}.
pub async fn cancel_download(id: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    client
        .post(format!("{}/cancel/{}", SERVER_BASE, id))
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;
    Ok(())
}

/// Subscribe to progress updates via SSE (GET /progress/{id}).
/// Calls `on_snapshot` with each new `ProgressSnapshot` until the download
/// is done or the connection drops.
pub async fn subscribe_progress<F>(id: &str, mut on_snapshot: F) -> Result<(), String>
where
    F: FnMut(ProgressSnapshot),
{
    use futures::StreamExt;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/progress/{}", SERVER_BASE, id))
        .send()
        .await
        .map_err(|e| format!("SSE connect error: {}", e))?;

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("SSE stream error: {}", e))?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // SSE lines are separated by \n; data lines start with "data:"
        loop {
            if let Some(newline_pos) = buf.find('\n') {
                let line = buf[..newline_pos].trim().to_string();
                buf = buf[newline_pos + 1..].to_string();

                if let Some(json_str) = line.strip_prefix("data:") {
                    let json_str = json_str.trim();
                    if let Ok(snap) = serde_json::from_str::<ProgressSnapshot>(json_str) {
                        let done = snap.done;
                        on_snapshot(snap);
                        if done {
                            return Ok(());
                        }
                    }
                }
            } else {
                break;
            }
        }
    }

    Ok(())
}
