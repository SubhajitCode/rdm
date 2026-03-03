use std::collections::HashMap;
use std::path::PathBuf;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, COOKIE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentState {
    NotStarted,
    Finished,
    Downloading,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamType {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    pub offset: i64,
    pub length: i64,
    pub downloaded: i64,
    pub state: SegmentState,
    pub stream_type: StreamType,
}

impl Segment {
    pub fn new(id: String, offset: i64, length: i64) -> Self {
        Self {
            id,
            offset,
            length,
            downloaded: 0,
            state: SegmentState::NotStarted,
            stream_type: StreamType::Primary,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub resumable: bool,
    pub resource_size: Option<u64>,
    pub final_uri: String,
    pub attachment_name: Option<String>,
    pub content_type: Option<String>,
    pub last_modified: Option<String>,
    pub max_connections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderData {
    pub headers: HashMap<String, Vec<String>>,
    pub cookies: Option<String>,
    pub url: String,
    pub authentication: Option<AuthenticationInfo>,
    pub proxy: Option<ProxyInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticationInfo {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloaderState {
    pub id: String,
    pub url: String,
    pub output_path: Option<String>,
    pub temp_dir: String,
    pub file_size: i64,
    pub headers: HashMap<String, Vec<String>>,
    pub cookies: Option<String>,
    pub authentication: Option<AuthenticationInfo>,
    pub proxy: Option<ProxyInfo>,
    pub convert_to_mp3: bool,
    pub last_modified: Option<String>,
    pub resumable: bool,
    pub attachment_name: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("disk error: {0}")]
    Disk(#[from] std::io::Error),
    #[error("invalid state")]
    InvalidState,
    #[error("max retry exceeded")]
    MaxRetryExceeded,
    #[error("non-resumable")]
    NonResumable,
    #[error("cancelled")]
    Cancelled,
    #[error("segment failed: {0}")]
    SegmentFailed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub segment_id: String,
    pub bytes_delta: u64,
    pub total_bytes: Option<u64>,
}

pub fn hashmap_vec_to_header_map(headers: &HashMap<String, Vec<String>>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, vals) in headers {
        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            for v in vals {
                if let Ok(value) = HeaderValue::from_str(v) {
                    map.append(name.clone(), value);
                }
            }
        }
    }
    map
}

impl ProxyInfo {
    pub fn to_reqwest_proxy(&self) -> reqwest::Proxy {
        let proxy_type = match self.port {
            80 => "http",
            443 => "https",
            _ => panic!("Unsupported proxy type: {}", self.port),
        };
        let proxy_url = format!("{}://{}:{}", proxy_type, self.host, self.port);
        //TODO handle proxy authentication later
        reqwest::Proxy::all(&proxy_url).unwrap()
    }
}

impl DownloaderState {
    /// Convenience constructor. Generates a UUID id and a temp dir automatically.
    pub fn new(url: String, output_path: PathBuf) -> Self {
        let id = Uuid::new_v4().to_string();
        let temp_dir = std::env::temp_dir().join(&id).to_string_lossy().to_string();
        Self {
            id,
            url,
            output_path: Some(output_path.to_string_lossy().to_string()),
            temp_dir,
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
        }
    }

    pub fn create_client(&self) -> Client {
        let mut builder = Client::builder();
        if let Some(proxy_info) = &self.proxy {
            let proxy = proxy_info.to_reqwest_proxy();
            builder = builder.proxy(proxy);
        }
        let mut default_headers = hashmap_vec_to_header_map(&self.headers);
        if let Some(_auth) = &self.authentication {
            let uname = _auth.username.clone();
            let pwd = _auth.password.clone();
            let auth_header = format!("{}:{}", uname, pwd);
            let auth_header_value = HeaderValue::from_str(&auth_header).unwrap();
            default_headers.insert(AUTHORIZATION, auth_header_value);
        }

        if let Some(cookie) = &self.cookies {
            //insert the cookie into the header map
            let cookie_header_value = HeaderValue::from_str(cookie).unwrap();
            default_headers.insert(COOKIE, cookie_header_value);
        }
        builder = builder.default_headers(default_headers);
        builder.build().unwrap()
    }
}
