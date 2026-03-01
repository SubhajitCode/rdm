use std::sync::Arc;

use tokio::sync::mpsc;

use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::progress::notifier::ProgressNotifier;
use crate::progress::observer::ProgressObserver;
use crate::types::types::DownloadError;

pub struct HttpDownloader {
    download_strategy: Arc<dyn DownloadStrategy>,
    notifier: ProgressNotifier,
}

impl HttpDownloader {
    pub fn new(strategy: Arc<dyn DownloadStrategy>) -> Self {
        Self {
            download_strategy: strategy,
            notifier: ProgressNotifier::new(),
        }
    }

    /// Register a progress observer. Must be called before `download()`.
    pub fn add_observer(&mut self, observer: Box<dyn ProgressObserver>) {
        self.notifier.add_observer(observer);
    }

    /// Run the full download lifecycle (preprocess → download → postprocess).
    ///
    /// Internally creates the progress channel, injects the sender into the
    /// strategy, runs the `ProgressNotifier` as a background task, then awaits
    /// it after the download completes.  Callers only need `add_observer`.
    pub async fn download(&mut self) -> Result<(), DownloadError> {
        let (progress_tx, progress_rx) = mpsc::channel(256);
        self.download_strategy.set_progress_tx(progress_tx);

        let notifier = std::mem::replace(&mut self.notifier, ProgressNotifier::new());
        let notifier_handle = tokio::spawn(async move {
            notifier.run(progress_rx).await;
        });

        let result = async {
            self.download_strategy.preprocess().await?;
            self.download_strategy.download().await?;
            self.download_strategy.postprocess().await
        }
        .await;

        // Clear the sender so the channel closes and the notifier task exits cleanly.
        self.download_strategy.clear_progress_tx();
        let _ = notifier_handle.await;

        result
    }

    pub async fn stop(&self) -> Result<(), DownloadError> {
        self.download_strategy.stop().await
    }

    pub async fn pause(&self) -> Result<(), DownloadError> {
        self.download_strategy.pause().await
    }
}
