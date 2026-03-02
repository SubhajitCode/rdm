use std::sync::Arc;

use reqwest::Client;
use tokio::sync::mpsc;

use crate::downloader::segment_grabber::probe_url;
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use crate::downloader::strategy::onepart_download_strategy::OnePartDownloadStrategy;
use crate::progress::notifier::ProgressNotifier;
use crate::progress::observer::ProgressObserver;
use crate::types::types::{DownloadError, DownloaderState, HeaderData};

pub struct HttpDownloader {
    download_strategy: Option<Arc<dyn DownloadStrategy>>,
    notifier: ProgressNotifier,
    downloader_state: DownloaderState,
    connections: usize,
}

impl HttpDownloader {
    pub fn new(downloader_state: DownloaderState, connections: usize) -> Self {
        Self {
            notifier: ProgressNotifier::new(),
            download_strategy: None,
            downloader_state,
            connections,
        }
    }

    /// Register a progress observer. Must be called before `download()`.
    pub fn add_observer(&mut self, observer: Box<dyn ProgressObserver>) {
        self.notifier.add_observer(observer);
    }

    /// Run the full download lifecycle (preprocess → download → postprocess).
    ///
    /// Performs a single HTTP probe to determine whether the server supports
    /// range requests, then selects `MultipartDownloadStrategy` or
    /// `OnePartDownloadStrategy` accordingly.  The probe result is passed
    /// directly into the strategy so `preprocess()` does not repeat the probe.
    pub async fn download(&mut self) -> Result<(), DownloadError> {
        self.download_strategy = Some(self.select_strategy().await?);

        let (progress_tx, progress_rx) = mpsc::channel(256);
        self.download_strategy.as_ref().unwrap().set_progress_tx(progress_tx);

        let notifier = std::mem::replace(&mut self.notifier, ProgressNotifier::new());
        let notifier_handle = tokio::spawn(async move {
            notifier.run(progress_rx).await;
        });

        let result = async {
            self.download_strategy.as_ref().unwrap().preprocess().await?;
            self.download_strategy.as_ref().unwrap().download().await?;
            self.download_strategy.as_ref().unwrap().postprocess().await
        }
        .await;

        self.download_strategy.as_ref().unwrap().clear_progress_tx();
        let _ = notifier_handle.await;
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
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(DownloadError::Network)?;

        let header_data = HeaderData {
            url: self.downloader_state.url.clone(),
            headers: self.downloader_state.headers.clone(),
            cookies: self.downloader_state.cookies.clone(),
            authentication: self.downloader_state.authentication.clone(),
            proxy: self.downloader_state.proxy.clone(),
        };

        let probe = probe_url(&client, &header_data).await?;

        log::info!(
            "[select_strategy] resumable={}, file_size={:?}",
            probe.resumable,
            probe.resource_size
        );

        let strategy: Arc<dyn DownloadStrategy> = if probe.resumable {
            Arc::new(MultipartDownloadStrategy::from_probe(
                self.downloader_state.clone(),
                probe,
                self.connections,
            ))
        } else {
            Arc::new(OnePartDownloadStrategy::from_probe(
                self.downloader_state.clone(),
                probe,
            ))
        };

        Ok(strategy)
    }
}
