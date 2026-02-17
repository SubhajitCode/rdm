use crate::rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use crate::rdm_core::types::types::DownloadError;
use std::sync::Arc;

pub struct HttpDownloader {
    download_strategy: Arc<dyn DownloadStrategy>,
}

impl HttpDownloader {
    pub fn new(strategy: Arc<dyn DownloadStrategy>) -> Self {
        Self {
            download_strategy: strategy,
        }
    }

    pub async fn download(&self) -> Result<(), DownloadError> {
        self.download_strategy.preprocess().await?;
        self.download_strategy.download().await?;
        self.download_strategy.postprocess().await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), DownloadError> {
        self.download_strategy.stop().await
    }

    pub async fn pause(&self) -> Result<(), DownloadError> {
        self.download_strategy.pause().await
    }
}
