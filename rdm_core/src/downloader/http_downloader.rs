use std::sync::Arc;

use tokio::sync::mpsc;

use crate::downloader::segment_grabber::{probe_url};
use crate::downloader::strategy::download_strategy::{DownloadStrategy, DownloadStrategyFactory};
use crate::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use crate::downloader::strategy::onepart_download_strategy::OnePartDownloadStrategy;
use crate::progress::notifier::ProgressNotifier;
use crate::progress::observer::ProgressObserver;
use crate::types::types::{DownloadError, DownloadPhase, DownloaderState, ProbeResult};

/// Default strategy factory: selects `MultipartDownloadStrategy` for resumable
/// downloads and `OnePartDownloadStrategy` for non-resumable ones.
///
/// Inject a custom implementation via `HttpDownloader::with_strategy_factory` to
/// override strategy selection (e.g., in tests, or to support new download types).
pub struct DefaultStrategyFactory;

impl DownloadStrategyFactory for DefaultStrategyFactory {
    fn create(
        &self,
        state: DownloaderState,
        probe: ProbeResult,
        connections: usize,
    ) -> Arc<dyn DownloadStrategy> {
        if probe.resumable {
            Arc::new(MultipartDownloadStrategy::from_probe(state, probe, connections))
        } else {
            Arc::new(OnePartDownloadStrategy::from_probe(state, probe))
        }
    }
}

pub struct HttpDownloader {
    download_strategy: Option<Arc<dyn DownloadStrategy>>,
    notifier: Arc<ProgressNotifier>,
    downloader_state: DownloaderState,
    connections: usize,
}

impl HttpDownloader {
    pub fn new(downloader_state: DownloaderState, connections: usize) -> Self {
        Self {
            notifier: Arc::new(ProgressNotifier::new()),
            download_strategy: None,
            downloader_state,
            connections,
        }
    }

    /// Register a progress observer and return its ID (pass to `remove_observer` to deregister).
    /// Must be called before `download()`.
    pub fn add_observer(&mut self, observer: Box<dyn ProgressObserver>) -> usize {
        Arc::get_mut(&mut self.notifier).unwrap().add_observer(observer)
    }

    /// Run the full download lifecycle (preprocess → download → postprocess).
    ///
    /// Performs a single HTTP probe to determine whether the server supports
    /// range requests, then selects `MultipartDownloadStrategy` or
    /// `OnePartDownloadStrategy` accordingly.  The probe result is passed
    /// directly into the strategy so `preprocess()` does not repeat the probe.
    pub async fn download(&mut self) -> Result<(), DownloadError> {
        self.downloader_state.set_phase(DownloadPhase::Probing);
        self.download_strategy = Some(self.select_strategy().await?);

        let (progress_tx, progress_rx) = mpsc::channel(256);
        self.download_strategy.as_ref().unwrap().set_progress_tx(progress_tx);

        let notifier = Arc::get_mut(&mut self.notifier);
        let progress_future= notifier.unwrap().run(progress_rx);

        let result_future = async {
            self.download_strategy.as_ref().unwrap().preprocess().await?;
            self.download_strategy.as_ref().unwrap().download().await?;
            self.download_strategy.as_ref().unwrap().postprocess().await
        };
        let result = tokio::join!(progress_future, result_future).1;
        self.download_strategy.as_ref().unwrap().clear_progress_tx();

        match &result {
            Ok(()) => self.downloader_state.set_phase(DownloadPhase::Complete),
            Err(e) => self.downloader_state.set_phase(DownloadPhase::Failed(e.to_string())),
        }
        result
    }

    pub async fn stop(&self) -> Result<(), DownloadError> {
        self.download_strategy.as_ref().unwrap().stop().await
    }

    pub async fn pause(&self) -> Result<(), DownloadError> {
        self.download_strategy.as_ref().unwrap().pause().await
    }

    /// Probe the URL once, then select and construct the appropriate strategy.
    async fn select_strategy(&self) -> Result<Arc<dyn DownloadStrategy>, DownloadError> {
        // Build the client from DownloaderState so proxy/auth/custom headers are
        // applied consistently for both the probe and the real download.
        let client = self.downloader_state.create_client();

        let probe = probe_url(&client, &self.downloader_state.url).await?;

        log::info!(
            "[select_strategy] resumable={}, file_size={:?}",
            probe.resumable,
            probe.resource_size
        );

        let strategy = {
            // Default: multipart for resumable, onepart otherwise.

            let default_factory = DefaultStrategyFactory;
            default_factory.create(self.downloader_state.clone(), probe, self.connections)
        };

        Ok(strategy)
    }
}
