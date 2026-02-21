use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use tokio::sync::mpsc;

use rdm_core::downloader::http_downloader::HttpDownloader;
use rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;

#[derive(Parser)]
#[command(name = "rdm", about = "Rust Download Manager")]
struct Args {
    /// URL to download
    #[arg(short, long, default_value = "https://proof.ovh.net/files/1Mb.dat")]
    url: String,

    /// Output file path
    #[arg(short, long, default_value = "downloaded_file")]
    output: PathBuf,
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = Args::parse();
    let url = args.url;
    let output_path = args.output;

    // Channel for progress events
    let (progress_tx, mut progress_rx) = mpsc::channel(256);

    // Create the strategy and downloader
    let strategy = Arc::new(MultipartDownloadStrategy::new(
        url.clone(),
        output_path,
        progress_tx,
    ));
    let downloader = HttpDownloader::new(strategy.clone());

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

    let result = downloader.download().await;

    // Drop the strategy (and its progress_tx sender) so the progress
    // receiver task can finish when the channel is drained.
    drop(downloader);
    drop(strategy);

    match result {
        Ok(()) => {
            let elapsed = start.elapsed();
            println!("Download completed in {:.2}s", elapsed.as_secs_f64());
        }
        Err(e) => {
            eprintln!("Download failed: {}", e);
        }
    }

    // Wait for progress printer to drain
    let _ = progress_handle.await;
}
