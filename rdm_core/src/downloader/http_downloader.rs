use std::sync::Arc;

use tokio::sync::mpsc;

use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use crate::downloader::util::detect_download_strategy;
use crate::progress::notifier::ProgressNotifier;
use crate::progress::observer::ProgressObserver;
use crate::types::types::{DownloadError, DownloaderState};

pub struct HttpDownloader {
    download_strategy: Option<Arc<dyn DownloadStrategy>>,
    notifier: ProgressNotifier,
    downloader_state: DownloaderState,
    connections:usize
}

impl HttpDownloader {
    pub  fn new(downloader_state: DownloaderState,connections:usize) -> Self {
        Self {
            notifier: ProgressNotifier::new(),
            download_strategy: None,
            downloader_state,
            connections
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
        // Inject the sender into the strategy so it can report progress during download.
        self.download_strategy = Some(self.create_download_strategy().await);
        self.download_strategy.as_ref().unwrap().set_progress_tx(progress_tx);
        let notifier = std::mem::replace(&mut self.notifier, ProgressNotifier::new());
        let notifier_handle = tokio::spawn(async move {
            notifier.run(progress_rx).await;
        });

        let result = async {
            self.download_strategy.as_ref().unwrap().preprocess().await?;
            self.download_strategy.as_ref().unwrap().download().await?;
            self.download_strategy.as_ref().unwrap().postprocess().await
        }.await;
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
    async  fn create_download_strategy(&self) -> Arc<dyn DownloadStrategy> {
        let download_type= detect_download_strategy(self.downloader_state.clone()).await;
        match download_type {
            crate::downloader::util::DownloadStrategyType::OnePart => {
                //TODO create OnePartDownloadStrategy
                todo!("OnePart download strategy")
            },
            crate::downloader::util::DownloadStrategyType::MultiPart => {
                //TODO create MultiPartDownloadStrategy
                Arc::new( MultipartDownloadStrategy::from_state(self.downloader_state.clone(),self.connections) )

            },
            crate::downloader::util::DownloadStrategyType::ERR(err) => {
                println!("Error detecting download strategy: {}", err);
                //TODO handle error properly
                panic!("Error detecting download strategy");
            }
        }
    }
}


