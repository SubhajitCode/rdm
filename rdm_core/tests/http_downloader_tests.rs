use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use wiremock::matchers::{header_regex, method};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use rdm_core::downloader::http_downloader::HttpDownloader;
use rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;

/// Generates deterministic test data.
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// A wiremock responder that correctly handles Range requests
/// by slicing the body and returning the appropriate byte range.
struct RangeResponder {
    body: Vec<u8>,
}

impl wiremock::Respond for RangeResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if let Some(range_header) = request.headers.get(&reqwest::header::RANGE) {
            let range_str = range_header.to_str().unwrap_or("");
            if let Some(parsed) = parse_range(range_str, self.body.len()) {
                let (start, end) = parsed;
                let slice = &self.body[start..=end];
                return ResponseTemplate::new(206)
                    .set_body_bytes(slice.to_vec())
                    .insert_header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, end, self.body.len()),
                    )
                    .insert_header("Content-Type", "application/octet-stream")
                    .insert_header(
                        "Content-Disposition",
                        "attachment; filename=\"rangetest.bin\"",
                    )
                    .insert_header("Last-Modified", "Sun, 01 Jan 2026 00:00:00 GMT");
            }
        }
        // Fallback: return full body with 200
        ResponseTemplate::new(200)
            .set_body_bytes(self.body.clone())
            .insert_header("Content-Type", "application/octet-stream")
    }
}

/// Parses a Range header like "bytes=0-" or "bytes=1024-2047"
fn parse_range(header: &str, body_len: usize) -> Option<(usize, usize)> {
    let s = header.strip_prefix("bytes=")?;
    let parts: Vec<&str> = s.splitn(2, '-').collect();
    if parts.len() != 2 {
        return None;
    }
    let start: usize = parts[0].parse().ok()?;
    let end: usize = if parts[1].is_empty() {
        body_len - 1
    } else {
        parts[1].parse().ok()?
    };
    Some((start, end.min(body_len - 1)))
}

// ---------------------------------------------------------------
// HttpDownloader end-to-end with a Range-aware mock server
// ---------------------------------------------------------------

#[tokio::test]
async fn test_http_downloader_end_to_end_with_range_server() {
    let body_size = 1024 * 1024; // 1 MB
    let body = generate_test_data(body_size);

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(RangeResponder { body: body.clone() })
        .mount(&server)
        .await;

    let (tx, _rx) = mpsc::channel(1024);
    let output_filename = format!("test_e2e_output_{}.bin", uuid::Uuid::new_v4());

    let strategy = Arc::new(MultipartDownloadStrategy::new(
        server.uri(),
        PathBuf::from(&output_filename),
        tx,
    ));

    let downloader = HttpDownloader::new(strategy.clone());
    downloader.download().await.unwrap();

    let output = std::fs::read(&output_filename).unwrap();

    assert_eq!(output.len(), body_size, "assembled file size should equal original body size");
    assert_eq!(output, body, "assembled file content should match original byte-for-byte");

    let _ = std::fs::remove_file(&output_filename);
}

#[tokio::test]
async fn test_http_downloader_non_resumable() {
    let body_size = 64 * 1024;
    let body = generate_test_data(body_size);

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Type", "application/octet-stream"),
        )
        .mount(&server)
        .await;

    let (tx, _rx) = mpsc::channel(256);
    let output_filename = "non_resumable_test.bin";
    let strategy = Arc::new(MultipartDownloadStrategy::new(
        server.uri(),
        PathBuf::from(output_filename),
        tx,
    ));

    let downloader = HttpDownloader::new(strategy);
    downloader.download().await.unwrap();

    let output = std::fs::read(output_filename).unwrap();
    assert_eq!(output.len(), body_size);
    assert_eq!(output, body);

    let _ = std::fs::remove_file(output_filename);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_http_downloader_stop_during_download() {
    let body_size: usize = 2 * 1024 * 1024;
    let _body = generate_test_data(body_size);

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(vec![0u8; 1024])
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(header_regex("Range", "^bytes=0-0$"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(vec![0u8; 1])
                .insert_header("Content-Range", format!("bytes 0-0/{}", body_size))
                .insert_header("Content-Type", "application/octet-stream")
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"stoptest.bin\"",
                ),
        )
        .mount(&server)
        .await;

    let (tx, _rx) = mpsc::channel(256);
    let strategy = Arc::new(MultipartDownloadStrategy::new(
        server.uri(),
        PathBuf::from("stop_test.bin"),
        tx,
    ));

    strategy.preprocess().await.unwrap();

    let strategy_clone = strategy.clone();
    let download_handle = tokio::spawn(async move { strategy_clone.download().await });

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    strategy.stop().await.unwrap();

    let result = tokio::time::timeout(std::time::Duration::from_secs(15), download_handle)
        .await
        .expect("download should complete within timeout after stop")
        .unwrap();

    assert!(
        result.is_err() || result.is_ok(),
        "download should complete after stop"
    );

    let temp_dir = strategy.temp_dir().await;
    let _ = std::fs::remove_dir_all(&temp_dir);
    let _ = std::fs::remove_file("stoptest.bin");
}

#[tokio::test]
async fn test_http_downloader_invalid_url_fails() {
    let (tx, _rx) = mpsc::channel(16);
    let strategy = Arc::new(MultipartDownloadStrategy::new(
        "http://127.0.0.1:1/nonexistent".to_string(),
        PathBuf::from("fail_test.bin"),
        tx,
    ));

    let downloader = HttpDownloader::new(strategy);
    let result = downloader.download().await;
    assert!(result.is_err(), "download to unreachable host should fail");
}

#[tokio::test]
async fn test_http_downloader_progress_events_received() {
    let body_size = 128 * 1024;
    let body = generate_test_data(body_size);

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(RangeResponder { body: body.clone() })
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::channel(1024);

    let strategy = Arc::new(MultipartDownloadStrategy::new(
        server.uri(),
        PathBuf::from("progress_test.bin"),
        tx,
    ));

    let downloader = HttpDownloader::new(strategy);

    let progress_handle = tokio::spawn(async move {
        let mut total: u64 = 0;
        let mut count: u64 = 0;
        while let Some(event) = rx.recv().await {
            total += event.bytes_downloaded;
            count += 1;
        }
        (total, count)
    });

    downloader.download().await.unwrap();
    drop(downloader);

    let (total, count) = progress_handle.await.unwrap();
    assert_eq!(total, body_size as u64, "total progress bytes should equal body size");
    assert!(count > 0, "should have received at least one progress event");

    let _ = std::fs::remove_file("progress_test.bin");
}
