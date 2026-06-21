use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};

use rdm_core::types::types::{DownloaderState, Segment};

use crate::sse_observer::EnrichedSnapshot;
use crate::types::{DownloadRequest, DownloadStatus, DownloadSummary};

#[derive(Debug, Clone)]
pub struct PersistedDownload {
    pub id: String,
    pub title: String,
    pub url: String,
    pub output_path: String,
    pub info: String,
    pub cookie: String,
    pub request_headers: HashMap<String, serde_json::Value>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub status: DownloadStatus,
    pub downloader_state: DownloaderState,
    pub segments: Vec<Segment>,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PersistedDownload {
    pub fn from_request(req: &DownloadRequest, downloader_state: DownloaderState) -> Self {
        Self {
            id: req.id.clone(),
            title: req.title.clone(),
            url: req.url.clone(),
            output_path: req.output_path.clone(),
            info: req.info.clone(),
            cookie: req.cookie.clone(),
            request_headers: req.request_headers.clone(),
            user_agent: req.user_agent.clone(),
            referer: req.referer.clone(),
            status: DownloadStatus::Queued,
            downloader_state,
            segments: Vec::new(),
            total_bytes_downloaded: 0,
            total_bytes: 0,
            speed: 0.0,
            eta_secs: 0.0,
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    pub fn to_summary(&self, is_active: bool) -> DownloadSummary {
        let output_path = PathBuf::from(&self.output_path);
        let file_exists = output_path.exists();
        let temp_exists = PathBuf::from(&self.downloader_state.temp_dir).exists();
        let can_resume = matches!(
            self.status,
            DownloadStatus::Stopped | DownloadStatus::Failed | DownloadStatus::Interrupted
        ) && self.downloader_state.resumable
            && temp_exists;

        DownloadSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            url: self.url.clone(),
            output_path: self.output_path.clone(),
            info: self.info.clone(),
            status: self.status,
            total_bytes_downloaded: self.total_bytes_downloaded,
            total_bytes: self.total_bytes,
            speed: self.speed,
            eta_secs: self.eta_secs,
            last_error: self.last_error.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            file_exists,
            temp_exists,
            can_resume,
            is_active,
        }
    }
}

#[derive(Clone)]
pub struct DownloadDatabase {
    path: PathBuf,
}

impl DownloadDatabase {
    pub fn new(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create db directory {:?}: {}", parent, e))?;
        }
        let db = Self { path };
        db.init()?;
        Ok(db)
    }

    pub fn init(&self) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS downloads (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                url TEXT NOT NULL,
                output_path TEXT NOT NULL,
                info TEXT NOT NULL DEFAULT '',
                cookie TEXT NOT NULL DEFAULT '',
                request_headers_json TEXT NOT NULL DEFAULT '{}',
                user_agent TEXT,
                referer TEXT,
                status TEXT NOT NULL,
                downloader_state_json TEXT NOT NULL,
                segments_json TEXT NOT NULL DEFAULT '[]',
                total_bytes_downloaded INTEGER NOT NULL DEFAULT 0,
                total_bytes INTEGER NOT NULL DEFAULT 0,
                speed REAL NOT NULL DEFAULT 0,
                eta_secs REAL NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .map_err(|e| format!("failed to initialize downloads table: {}", e))?;
        Ok(())
    }

    pub fn open(&self) -> Result<Connection, String> {
        Connection::open(&self.path).map_err(|e| format!("failed to open sqlite db {:?}: {}", self.path, e))
    }

    pub fn insert_download(&self, record: &PersistedDownload) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "INSERT OR REPLACE INTO downloads (
                id, title, url, output_path, info, cookie, request_headers_json, user_agent, referer,
                status, downloader_state_json, segments_json, total_bytes_downloaded, total_bytes,
                speed, eta_secs, last_error, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, CURRENT_TIMESTAMP)",
            params![
                record.id,
                record.title,
                record.url,
                record.output_path,
                record.info,
                record.cookie,
                serde_json::to_string(&record.request_headers).map_err(|e| e.to_string())?,
                record.user_agent,
                record.referer,
                record.status.as_str(),
                serde_json::to_string(&record.downloader_state).map_err(|e| e.to_string())?,
                serde_json::to_string(&record.segments).map_err(|e| e.to_string())?,
                record.total_bytes_downloaded as i64,
                record.total_bytes as i64,
                record.speed,
                record.eta_secs,
                record.last_error,
            ],
        )
        .map_err(|e| format!("failed to insert download {}: {}", record.id, e))?;
        Ok(())
    }

    pub fn update_runtime(
        &self,
        id: &str,
        status: DownloadStatus,
        downloader_state: &DownloaderState,
        segments: &[Segment],
        last_error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE downloads
             SET status = ?2,
                 downloader_state_json = ?3,
                 segments_json = ?4,
                 last_error = ?5,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![
                id,
                status.as_str(),
                serde_json::to_string(downloader_state).map_err(|e| e.to_string())?,
                serde_json::to_string(segments).map_err(|e| e.to_string())?,
                last_error,
            ],
        )
        .map_err(|e| format!("failed to update runtime for {}: {}", id, e))?;
        Ok(())
    }

    pub fn update_progress(&self, id: &str, snapshot: &EnrichedSnapshot) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE downloads
             SET total_bytes_downloaded = ?2,
                 total_bytes = ?3,
                 speed = ?4,
                 eta_secs = ?5,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![
                id,
                snapshot.total_bytes_downloaded as i64,
                snapshot.total_bytes as i64,
                snapshot.speed,
                snapshot.eta_secs,
            ],
        )
        .map_err(|e| format!("failed to update progress for {}: {}", id, e))?;
        Ok(())
    }

    pub fn update_status(
        &self,
        id: &str,
        status: DownloadStatus,
        last_error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE downloads
             SET status = ?2,
                 last_error = ?3,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![id, status.as_str(), last_error],
        )
        .map_err(|e| format!("failed to update status for {}: {}", id, e))?;
        Ok(())
    }

    pub fn mark_running_as_interrupted(&self) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute(
            "UPDATE downloads
             SET status = 'interrupted',
                 last_error = COALESCE(last_error, 'rdmd stopped while download was running'),
                 updated_at = CURRENT_TIMESTAMP
             WHERE status IN ('queued', 'running')",
            [],
        )
        .map_err(|e| format!("failed to mark interrupted downloads: {}", e))?;
        Ok(())
    }

    pub fn get_download(&self, id: &str) -> Result<Option<PersistedDownload>, String> {
        let conn = self.open()?;
        conn.query_row(
            "SELECT id, title, url, output_path, info, cookie, request_headers_json, user_agent, referer,
                    status, downloader_state_json, segments_json, total_bytes_downloaded, total_bytes,
                    speed, eta_secs, last_error, created_at, updated_at
             FROM downloads
             WHERE id = ?1",
            params![id],
            row_to_download,
        )
        .optional()
        .map_err(|e| format!("failed to load download {}: {}", id, e))
    }

    pub fn list_downloads(&self) -> Result<Vec<PersistedDownload>, String> {
        let conn = self.open()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, url, output_path, info, cookie, request_headers_json, user_agent, referer,
                        status, downloader_state_json, segments_json, total_bytes_downloaded, total_bytes,
                        speed, eta_secs, last_error, created_at, updated_at
                 FROM downloads
                 ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC",
            )
            .map_err(|e| format!("failed to prepare list query: {}", e))?;
        let rows = stmt
            .query_map([], row_to_download)
            .map_err(|e| format!("failed to query downloads: {}", e))?;

        let mut downloads = Vec::new();
        for row in rows {
            downloads.push(row.map_err(|e| format!("failed to decode download row: {}", e))?);
        }
        Ok(downloads)
    }

    pub fn delete_download(&self, id: &str) -> Result<(), String> {
        let conn = self.open()?;
        conn.execute("DELETE FROM downloads WHERE id = ?1", params![id])
            .map_err(|e| format!("failed to delete download {}: {}", id, e))?;
        Ok(())
    }
}

fn row_to_download(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersistedDownload> {
    let request_headers_json: String = row.get(6)?;
    let downloader_state_json: String = row.get(10)?;
    let segments_json: String = row.get(11)?;
    let status: String = row.get(9)?;

    Ok(PersistedDownload {
        id: row.get(0)?,
        title: row.get(1)?,
        url: row.get(2)?,
        output_path: row.get(3)?,
        info: row.get(4)?,
        cookie: row.get(5)?,
        request_headers: serde_json::from_str(&request_headers_json).unwrap_or_default(),
        user_agent: row.get(7)?,
        referer: row.get(8)?,
        status: DownloadStatus::from_str(&status).unwrap_or(DownloadStatus::Failed),
        downloader_state: serde_json::from_str(&downloader_state_json).map_err(json_err)?,
        segments: serde_json::from_str(&segments_json).map_err(json_err)?,
        total_bytes_downloaded: row.get::<_, i64>(12)?.max(0) as u64,
        total_bytes: row.get::<_, i64>(13)?.max(0) as u64,
        speed: row.get(14)?,
        eta_secs: row.get(15)?,
        last_error: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

fn json_err(err: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(err),
    )
}
