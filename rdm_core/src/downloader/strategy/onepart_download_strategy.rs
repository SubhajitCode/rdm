use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::mpsc::Sender;
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::types::types::{DownloadError, DownloaderState, ProgressEvent};

pub struct OnePartDownloadStrategy{
    client: Client,
    progress_rx: Sender<Result<ProgressEvent, String>>,
    downloader_state: DownloaderState
}

impl OnePartDownloadStrategy {
    pub fn new(client: Client, progress_rx: Sender<Result<ProgressEvent, String>>, downloader_state: DownloaderState) -> Self {
        Self {
            client,
            progress_rx,
            downloader_state
        }
    }
}

#[async_trait]
impl DownloadStrategy for OnePartDownloadStrategy {
    fn set_progress_tx(&self, tx: Sender<Result<ProgressEvent, String>>) {
        todo!()
    }

    fn clear_progress_tx(&self) {
        todo!()
    }

    async fn preprocess(&self) -> Result<(), DownloadError> {
        todo!()
    }

    async fn download(&self) -> Result<(), DownloadError> {
        // Implement the logic to download the file in one part
        // This is a placeholder implementation
        println!("Downloading {} in one part", &self.downloader_state.url);
        Ok(())
    }

    async fn pause(&self) -> Result<(), DownloadError> {
        todo!()
    }

    async fn stop(&self) -> Result<(), DownloadError> {
        todo!()
    }

    async fn postprocess(&self) -> Result<(), DownloadError> {
        todo!()
    }
}