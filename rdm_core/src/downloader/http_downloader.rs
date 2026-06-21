use std::sync::Arc;

use tokio::sync::mpsc;

use crate::downloader::segment_grabber::{probe_url};
use crate::downloader::strategy::download_strategy::{DownloadStrategy, DownloadStrategyFactory};
use crate::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use crate::downloader::strategy::onepart_download_strategy::OnePartDownloadStrategy;
use crate::progress::notifier::ProgressNotifier;
use crate::progress::observer::ProgressObserver;
use crate::types::types::{DownloadError, DownloadPhase, DownloaderState, ProbeResult, Segment};

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
    persisted_segments: Option<Vec<Segment>>,
}

impl HttpDownloader {
    pub fn new(downloader_state: DownloaderState, connections: usize) -> Self {
        Self {
            notifier: Arc::new(ProgressNotifier::new()),
            download_strategy: None,
            downloader_state,
            connections,
            persisted_segments: None,
        }
    }

    pub fn from_persisted(
        downloader_state: DownloaderState,
        connections: usize,
        segments: Vec<Segment>,
    ) -> Self {
        Self {
            notifier: Arc::new(ProgressNotifier::new()),
            download_strategy: None,
            downloader_state,
            connections,
            persisted_segments: Some(segments),
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
    pub async fn prepare(&mut self) -> Result<(), DownloadError> {
        if self.download_strategy.is_some() {
            return Ok(());
        }

        self.downloader_state.set_phase(DownloadPhase::Probing);
        self.download_strategy = Some(self.select_strategy().await?);
        self.download_strategy.as_ref().unwrap().preprocess().await?;
        Ok(())
    }

    pub async fn run_prepared(&mut self) -> Result<(), DownloadError> {
        if self.download_strategy.is_none() {
            return Err(DownloadError::InvalidState);
        }

        let (progress_tx, progress_rx) = mpsc::channel(256);
        self.download_strategy.as_ref().unwrap().set_progress_tx(progress_tx);

        let notifier = Arc::get_mut(&mut self.notifier);
        let progress_future= notifier.unwrap().run(progress_rx);

        let result_future = async {
            let result = async {
                self.download_strategy.as_ref().unwrap().download().await?;
                self.download_strategy.as_ref().unwrap().postprocess().await
            }
            .await;
            self.download_strategy.as_ref().unwrap().clear_progress_tx();
            result
        };
        let result = tokio::join!(progress_future, result_future).1;

        match &result {
            Ok(()) => self.downloader_state.set_phase(DownloadPhase::Complete),
            Err(e) => self.downloader_state.set_phase(DownloadPhase::Failed(e.to_string())),
        }
        result
    }

    pub async fn download(&mut self) -> Result<(), DownloadError> {
        self.prepare().await?;
        self.run_prepared().await
    }

    pub async fn stop(&self) -> Result<(), DownloadError> {
        let strategy = self.download_strategy.as_ref().ok_or(DownloadError::InvalidState)?;
        strategy.stop().await
    }

    pub async fn pause(&self) -> Result<(), DownloadError> {
        let strategy = self.download_strategy.as_ref().ok_or(DownloadError::InvalidState)?;
        strategy.pause().await
    }

    pub fn current_state(&self) -> DownloaderState {
        self.download_strategy
            .as_ref()
            .map(|strategy| strategy.current_state())
            .unwrap_or_else(|| self.downloader_state.clone())
    }

    pub fn strategy_handle(&self) -> Option<Arc<dyn DownloadStrategy>> {
        self.download_strategy.as_ref().map(Arc::clone)
    }

    pub async fn current_segments(&self) -> Vec<Segment> {
        if let Some(strategy) = &self.download_strategy {
            strategy.current_segments().await
        } else {
            self.persisted_segments.clone().unwrap_or_default()
        }
    }

    /// Probe the URL once, then select and construct the appropriate strategy.
    async fn select_strategy(&self) -> Result<Arc<dyn DownloadStrategy>, DownloadError> {
        if let Some(segments) = &self.persisted_segments {
            return Ok(Arc::new(MultipartDownloadStrategy::from_persisted(
                self.downloader_state.clone(),
                segments.clone(),
                self.connections,
            )));
        }

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
