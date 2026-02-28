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
        // Create the internal progress channel.
        let (progress_tx, progress_rx) = mpsc::channel(256);

        // Inject the sender into the strategy.
        self.download_strategy.set_progress_tx(progress_tx);

        // Take the notifier out so we can move it into the background task.
        // A fresh empty notifier is left in place so the field stays valid.
        let notifier = std::mem::replace(&mut self.notifier, ProgressNotifier::new());

        // Spawn the notifier — it drains until all senders are dropped.
        let notifier_handle = tokio::spawn(async move {
            notifier.run(progress_rx).await;
        });

        // Run the three-phase download.
        let result = async {
            self.download_strategy.preprocess().await?;
            self.download_strategy.download().await?;
            self.download_strategy.postprocess().await
        }
        .await;

        // Clear the sender held by the strategy so the channel closes and the
        // notifier task can call on_complete / on_error and exit cleanly.
        self.download_strategy.clear_progress_tx();

        // Wait for the notifier to finish before returning to the caller.
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
