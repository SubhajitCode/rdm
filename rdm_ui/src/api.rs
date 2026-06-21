use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResponse {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Running,
    Stopped,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DownloadSummary {
    pub id: String,
    pub title: String,
    pub url: String,
    pub output_path: String,
    pub info: String,
    pub status: DownloadStatus,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub file_exists: bool,
    pub temp_exists: bool,
    pub can_resume: bool,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentSnapshot {
    pub segment_id: String,
    pub offset: u64,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    #[serde(default)]
    pub segments: Vec<SegmentSnapshot>,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
    pub done: bool,
}

const SERVER_BASE: &str = "http://127.0.0.1:8597";

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

    let body = resp
        .json::<DownloadResponse>()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if body.status.starts_with("error:") {
        return Err(body.status.clone());
    }

    Ok(body)
}

pub async fn stop_download(id: &str) -> Result<(), String> {
    simple_post(format!("{}/downloads/{}/stop", SERVER_BASE, id)).await
}

pub async fn resume_download(id: &str) -> Result<(), String> {
    simple_post(format!("{}/downloads/{}/resume", SERVER_BASE, id)).await
}

pub async fn delete_download_entry(id: &str) -> Result<(), String> {
    simple_delete(format!("{}/downloads/{}", SERVER_BASE, id)).await
}

pub async fn delete_download_with_files(id: &str) -> Result<(), String> {
    simple_delete(format!("{}/downloads/{}/files", SERVER_BASE, id)).await
}

pub async fn list_downloads() -> Result<Vec<DownloadSummary>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/downloads", SERVER_BASE))
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Server returned status {}", resp.status()));
    }

    resp.json::<Vec<DownloadSummary>>()
        .await
        .map_err(|e| format!("Parse error: {}", e))
}

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

        loop {
            if let Some(newline_pos) = buf.find('\n') {
                let line = buf[..newline_pos].trim().to_string();
                buf = buf[newline_pos + 1..].to_string();

                if let Some(json_str) = line.strip_prefix("data:") {
                    if let Ok(snap) = serde_json::from_str::<ProgressSnapshot>(json_str.trim()) {
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

async fn simple_post(url: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Server returned status {}", resp.status()));
    }
    Ok(())
}

async fn simple_delete(url: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let resp = client
        .delete(url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Server returned status {}", resp.status()));
    }
    Ok(())
}
