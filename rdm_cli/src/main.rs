use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;

use rdm_core::downloader::http_downloader::HttpDownloader;
use rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;

mod terminal_observer;
use terminal_observer::TerminalProgressObserver;

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

    let strategy = Arc::new(MultipartDownloadStrategy::new(url.clone(), output_path));
    let mut downloader = HttpDownloader::new(strategy);
    downloader.add_observer(Box::new(TerminalProgressObserver::new()));

    println!("Starting download: {}", url);
    let start = Instant::now();

    match downloader.download().await {
        Ok(()) => {
            let elapsed = start.elapsed();
            println!("Download completed in {:.2}s", elapsed.as_secs_f64());
        }
        Err(e) => {
            eprintln!("Download failed: {}", e);
        }
    }
}
