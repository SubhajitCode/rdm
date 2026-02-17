use crate::rdm_core::types::types::DownloadError;
use async_trait::async_trait;

#[async_trait]
pub trait DownloadStrategy: Send + Sync {
    async fn preprocess(&self) -> Result<(), DownloadError>;
    async fn download(&self) -> Result<(), DownloadError>;
    async fn pause(&self) -> Result<(), DownloadError>;
    async fn stop(&self) -> Result<(), DownloadError>;
    async fn postprocess(&self) -> Result<(), DownloadError>;
}
