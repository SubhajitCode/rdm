use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use rdm::rdm_core::downloader::http_downloader::HttpDownloader;
use rdm::rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;

#[tokio::main]
async fn main() {
    // OVH test file: 1 MB, supports Range requests (resumable)
    let url = "https://proof.ovh.net/files/1Mb.dat".to_string();
    let output_path = PathBuf::from("downloaded_1MB.dat");

    // Channel for progress events
    let (progress_tx, mut progress_rx) = mpsc::channel(256);

    // Create the strategy and downloader
    let strategy = Arc::new(MultipartDownloadStrategy::new(
        url.clone(),
        output_path,
        progress_tx,
    ));
    let downloader = HttpDownloader::new(strategy);

    // Spawn a task to print progress
    let progress_handle = tokio::spawn(async move {
        let mut total_downloaded: u64 = 0;
        while let Some(event) = progress_rx.recv().await {
            total_downloaded += event.bytes_downloaded;
            let kb = total_downloaded as f64 / 1024.0;
            eprint!("\r  Downloaded: {:.1} KB", kb);
        }
        eprintln!();
    });

    println!("Starting download: {}", url);
    let start = Instant::now();

    match downloader.download().await {
        Ok(()) => {
            let elapsed = start.elapsed();
            println!(
                "Download completed in {:.2}s",
                elapsed.as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("Download failed: {}", e);
        }
    }

    // Wait for progress printer to drain
    let _ = progress_handle.await;
}
