use crate::types::types::DownloaderState;

pub enum DownloadStrategyType{
    OnePart,
    MultiPart,
    ERR(String)
}


pub async  fn detect_download_strategy(downloader_state: DownloaderState) -> DownloadStrategyType {
   let client = downloader_state.get_client();
    let req = client.get(&downloader_state.url).header("Range", "bytes=0-0");
    return if let Ok(resp) = req.send().await {
        if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            drop(resp);
            log::info!("[detect_download_strategy] MultiPart");
            DownloadStrategyType::MultiPart
        } else {
            drop(resp);
            log::info!("[detect_download_strategy] OnePart");
            DownloadStrategyType::OnePart
        }
    } else {
        DownloadStrategyType::ERR(format!("Failed to send request: {}", downloader_state.url))
    }
}


