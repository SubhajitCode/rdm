use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::types::types::{DownloadError, DownloaderState, ProgressEvent, ProbeResult};

#[async_trait]
pub trait DownloadStrategy: Send + Sync {
    /// Inject the progress sender before calling `download()`.
    /// The `HttpDownloader` calls this internally; callers never touch the channel.
    fn set_progress_tx(&self, tx: mpsc::Sender<Result<ProgressEvent, String>>);

    /// Drop the progress sender so the notifier channel closes after download.
    fn clear_progress_tx(&self);

    async fn preprocess(&self) -> Result<(), DownloadError>;
    async fn download(&self) -> Result<(), DownloadError>;
    async fn pause(&self) -> Result<(), DownloadError>;
    async fn stop(&self) -> Result<(), DownloadError>;
    async fn postprocess(&self) -> Result<(), DownloadError>;
}

/// Factory that selects and constructs the appropriate `DownloadStrategy`
/// based on a completed `ProbeResult`.
///
/// Implement this trait to inject custom strategies (e.g. a streaming strategy,
/// a BitTorrent strategy, or a mock for testing).
pub trait DownloadStrategyFactory: Send + Sync {
    fn create(
        &self,
        state: DownloaderState,
        probe: ProbeResult,
        connections: usize,
    ) -> Arc<dyn DownloadStrategy>;
}
