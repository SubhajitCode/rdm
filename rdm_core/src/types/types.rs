use std::collections::HashMap;
use std::path::PathBuf;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, COOKIE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents the current phase of a download's lifecycle.
///
/// Transitions follow the sequence: `Probing → Segmenting → Downloading → Assembling → Complete`.
/// Any phase can transition to `Failed`. This enum is stored in `DownloaderState.phase` so that
/// callers can observe and log state transitions explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DownloadPhase {
    /// Probing the URL to determine file size, resumability, and metadata.
    Probing,
    /// Metadata received; creating download segments.
    Segmenting,
    /// Actively downloading (progress 0.0–1.0; `None` if size is unknown).
    Downloading { progress: Option<f32> },
    /// All segments downloaded; assembling the final output file.
    Assembling,
    /// Download finished successfully.
    Complete,
    /// Download failed with an error message.
    Failed(String),
}

impl DownloadPhase {
    /// Returns `true` if this phase is terminal (no further transitions expected).
    pub fn is_terminal(&self) -> bool {
        matches!(self, DownloadPhase::Complete | DownloadPhase::Failed(_))
    }
}

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
    /// Current phase in the download lifecycle. Starts at `Probing`.
    pub phase: DownloadPhase,
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
    /// Byte offset of this segment within the file (used to label segment bars in the UI).
    pub offset: u64,
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
    pub fn to_reqwest_proxy(&self) -> Result<reqwest::Proxy, String> {
        // Infer scheme from well-known ports; default to HTTP for everything else.
        let scheme = match self.port {
            80 | 8080 | 3128 => "http",
            443 => "https",
            1080 => "socks5",
            other => {
                log::warn!(
                    "Unknown proxy port {}; assuming HTTP scheme. \
                     Set port to 80/443/1080 to suppress this warning.",
                    other
                );
                "http"
            }
        };
        let proxy_url = format!("{}://{}:{}", scheme, self.host, self.port);
        let mut proxy = reqwest::Proxy::all(&proxy_url)
            .map_err(|e| format!("Invalid proxy URL \"{}\": {}", proxy_url, e))?;

        if let (Some(user), Some(pass)) = (&self.username, &self.password) {
            proxy = proxy.basic_auth(user, pass);
        }
        Ok(proxy)
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
            phase: DownloadPhase::Probing,
        }
    }

    /// Transition to a new phase, logging the change.
    pub fn set_phase(&mut self, phase: DownloadPhase) {
        log::debug!("[{}] phase: {:?} → {:?}", self.id, self.phase, phase);
        self.phase = phase;
    }

    pub fn create_client(&self) -> Client {
        let mut builder = Client::builder();
        if let Some(proxy_info) = &self.proxy {
            match proxy_info.to_reqwest_proxy() {
                Ok(proxy) => {
                    builder = builder.proxy(proxy);
                }
                Err(e) => {
                    log::warn!(
                        "Proxy configuration error — downloading without proxy: {}",
                        e
                    );
                }
            }
        }
        let mut default_headers = hashmap_vec_to_header_map(&self.headers);
        if let Some(auth) = &self.authentication {
            use base64::{engine::general_purpose, Engine as _};
            let credentials = format!("{}:{}", auth.username, auth.password);
            let encoded = general_purpose::STANDARD.encode(credentials.as_bytes());
            let header_value = format!("Basic {}", encoded);
            match HeaderValue::from_str(&header_value) {
                Ok(v) => {
                    default_headers.insert(AUTHORIZATION, v);
                }
                Err(e) => {
                    log::warn!("Could not set Authorization header: {}", e);
                }
            }
        }

        if let Some(cookie) = &self.cookies {
            match HeaderValue::from_str(cookie) {
                Ok(v) => {
                    default_headers.insert(COOKIE, v);
                }
                Err(e) => {
                    log::warn!("Could not set Cookie header: {}", e);
                }
            }
        }
        builder = builder.default_headers(default_headers);
        builder.build().unwrap()
    }
}

/// Fluent builder for `DownloaderState`.
///
/// Use this for constructing states with multiple optional fields (proxy, auth,
/// cookies, custom headers) without relying on direct field mutation.
///
/// # Example
/// ```no_run
/// use std::path::PathBuf;
/// use rdm_core::types::types::DownloaderStateBuilder;
///
/// let state = DownloaderStateBuilder::new(
///         "https://example.com/file.zip".into(),
///         PathBuf::from("/tmp/file.zip"),
///     )
///     .with_cookies("session=abc123".into())
///     .with_convert_to_mp3(false)
///     .build();
/// ```
pub struct DownloaderStateBuilder {
    url: String,
    output_path: PathBuf,
    headers: HashMap<String, Vec<String>>,
    cookies: Option<String>,
    authentication: Option<AuthenticationInfo>,
    proxy: Option<ProxyInfo>,
    convert_to_mp3: bool,
}

impl DownloaderStateBuilder {
    pub fn new(url: String, output_path: PathBuf) -> Self {
        Self {
            url,
            output_path,
            headers: HashMap::new(),
            cookies: None,
            authentication: None,
            proxy: None,
            convert_to_mp3: false,
        }
    }

    pub fn with_headers(mut self, headers: HashMap<String, Vec<String>>) -> Self {
        self.headers = headers;
        self
    }

    pub fn add_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), vec![value.into()]);
        self
    }

    pub fn with_cookies(mut self, cookies: String) -> Self {
        self.cookies = Some(cookies);
        self
    }

    pub fn with_auth(mut self, auth: AuthenticationInfo) -> Self {
        self.authentication = Some(auth);
        self
    }

    pub fn with_proxy(mut self, proxy: ProxyInfo) -> Self {
        self.proxy = Some(proxy);
        self
    }

    pub fn with_convert_to_mp3(mut self, convert: bool) -> Self {
        self.convert_to_mp3 = convert;
        self
    }

    /// Build the `DownloaderState`. Automatically assigns a UUID id and temp directory.
    pub fn build(self) -> DownloaderState {
        let mut state = DownloaderState::new(self.url, self.output_path);
        state.headers = self.headers;
        state.cookies = self.cookies;
        state.authentication = self.authentication;
        state.proxy = self.proxy;
        state.convert_to_mp3 = self.convert_to_mp3;
        state
    }
}
