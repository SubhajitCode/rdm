use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use reqwest::Client;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rdm::rdm_core::downloader::piece_grabber::{download_piece, extract_filename, probe_url};
use rdm::rdm_core::types::types::{DownloadError, HeaderData, Piece, SegmentState};

/// Helper: creates a minimal HeaderData pointing at the given URL.
fn make_header_data(url: &str) -> HeaderData {
    HeaderData {
        url: url.to_string(),
        headers: HashMap::new(),
        cookies: None,
        authentication: None,
        proxy: None,
    }
}

// ---------------------------------------------------------------
// extract_filename
// ---------------------------------------------------------------

#[test]
fn test_extract_filename_quoted() {
    let result = extract_filename("attachment; filename=\"report.pdf\"");
    assert_eq!(result, Some("report.pdf".to_string()));
}

#[test]
fn test_extract_filename_unquoted() {
    let result = extract_filename("attachment; filename=data.csv");
    assert_eq!(result, Some("data.csv".to_string()));
}

#[test]
fn test_extract_filename_with_trailing_params() {
    let result = extract_filename("attachment; filename=\"image.png\"; size=1024");
    assert_eq!(result, Some("image.png".to_string()));
}

#[test]
fn test_extract_filename_missing() {
    let result = extract_filename("inline");
    assert_eq!(result, None);
}

// ---------------------------------------------------------------
// probe_url
// ---------------------------------------------------------------

#[tokio::test]
async fn test_probe_resumable_server() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(header("Range", "bytes=0-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("Content-Length", "5242880")
                .insert_header("Content-Type", "application/octet-stream")
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"testfile.bin\"",
                )
                .insert_header("Last-Modified", "Mon, 01 Jan 2026 00:00:00 GMT"),
        )
        .mount(&server)
        .await;

    let client = Client::new();
    let header_data = make_header_data(&server.uri());

    let probe = probe_url(&client, &header_data).await.unwrap();

    assert!(probe.resumable);
    assert_eq!(probe.resource_size, Some(0)); // empty body, Content-Length header present but body is empty
    assert_eq!(probe.attachment_name, Some("testfile.bin".to_string()));
    assert_eq!(
        probe.content_type,
        Some("application/octet-stream".to_string())
    );
    assert_eq!(
        probe.last_modified,
        Some("Mon, 01 Jan 2026 00:00:00 GMT".to_string())
    );
    assert!(probe.final_uri.starts_with(&server.uri()));
}

#[tokio::test]
async fn test_probe_non_resumable_server() {
    let server = MockServer::start().await;

    // Server ignores Range header, returns 200
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("Content-Type", "text/plain"),
        )
        .mount(&server)
        .await;

    let client = Client::new();
    let header_data = make_header_data(&server.uri());

    let probe = probe_url(&client, &header_data).await.unwrap();

    assert!(!probe.resumable);
    assert_eq!(probe.attachment_name, None);
    assert_eq!(probe.content_type, Some("text/plain".to_string()));
    assert_eq!(probe.last_modified, None);
}

#[tokio::test]
async fn test_probe_network_error() {
    let client = Client::new();
    // Point to a port that nothing is listening on
    let header_data = make_header_data("http://127.0.0.1:1");

    let result = probe_url(&client, &header_data).await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------
// download_piece
// ---------------------------------------------------------------

#[tokio::test]
async fn test_download_piece_full_body() {
    let server = MockServer::start().await;
    let body = vec![0xABu8; 1024]; // 1 KB of 0xAB

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let client = Client::new();
    let header_data = make_header_data(&server.uri());
    let temp_dir = tempfile::tempdir().unwrap();
    let cancel_token = CancellationToken::new();

    // Non-resumable piece (length = -1)
    let piece = Piece::new("piece-full".to_string(), 0, -1);

    let progress = Arc::new(AtomicU64::new(0));
    let progress_clone = progress.clone();

    let result = download_piece(
        piece,
        &client,
        &header_data,
        temp_dir.path().to_path_buf(),
        cancel_token,
        move |bytes| {
            progress_clone.fetch_add(bytes, Ordering::Relaxed);
        },
    )
    .await;

    let finished_piece = result.unwrap();
    assert_eq!(finished_piece.state, SegmentState::Finished);
    assert_eq!(finished_piece.downloaded, 1024);
    assert_eq!(progress.load(Ordering::Relaxed), 1024);

    // Verify file content
    let file_content = std::fs::read(temp_dir.path().join("piece-full")).unwrap();
    assert_eq!(file_content, body);
}

#[tokio::test]
async fn test_download_piece_with_range() {
    let server = MockServer::start().await;
    let body = vec![0xCDu8; 512];

    Mock::given(method("GET"))
        .and(header("Range", "bytes=1024-1535"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let client = Client::new();
    let header_data = make_header_data(&server.uri());
    let temp_dir = tempfile::tempdir().unwrap();
    let cancel_token = CancellationToken::new();

    // Resumable piece with defined offset and length
    let piece = Piece::new("piece-range".to_string(), 1024, 512);

    let result = download_piece(
        piece,
        &client,
        &header_data,
        temp_dir.path().to_path_buf(),
        cancel_token,
        |_| {},
    )
    .await;

    let finished_piece = result.unwrap();
    assert_eq!(finished_piece.state, SegmentState::Finished);
    assert_eq!(finished_piece.downloaded, 512);

    let file_content = std::fs::read(temp_dir.path().join("piece-range")).unwrap();
    assert_eq!(file_content, body);
}

#[tokio::test]
async fn test_download_piece_cancellation() {
    let server = MockServer::start().await;

    // Respond with a delay so we have time to cancel
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0u8; 1024])
                .set_delay(std::time::Duration::from_secs(10)),
        )
        .mount(&server)
        .await;

    let client = Client::new();
    let header_data = make_header_data(&server.uri());
    let temp_dir = tempfile::tempdir().unwrap();
    let cancel_token = CancellationToken::new();

    let piece = Piece::new("piece-cancel".to_string(), 0, -1);

    // Cancel immediately before download starts
    cancel_token.cancel();

    let result = download_piece(
        piece,
        &client,
        &header_data,
        temp_dir.path().to_path_buf(),
        cancel_token,
        |_| {},
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        DownloadError::Cancelled => {} // expected
        other => panic!("expected Cancelled, got {:?}", other),
    }
}

#[tokio::test]
async fn test_download_piece_retries_on_failure() {
    let client = Client::new();
    // Point to a port nothing is listening on — immediate connection refused
    let header_data = make_header_data("http://127.0.0.1:1");
    let temp_dir = tempfile::tempdir().unwrap();
    let cancel_token = CancellationToken::new();

    let piece = Piece::new("piece-retry".to_string(), 0, -1);

    let result = download_piece(
        piece,
        &client,
        &header_data,
        temp_dir.path().to_path_buf(),
        cancel_token,
        |_| {},
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        DownloadError::MaxRetryExceeded => {} // expected after 3 retries
        other => panic!("expected MaxRetryExceeded, got {:?}", other),
    }
}

#[tokio::test]
async fn test_download_piece_progress_callback_called() {
    let server = MockServer::start().await;
    let body = vec![0xEFu8; 2048];

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;

    let client = Client::new();
    let header_data = make_header_data(&server.uri());
    let temp_dir = tempfile::tempdir().unwrap();
    let cancel_token = CancellationToken::new();

    let piece = Piece::new("piece-progress".to_string(), 0, -1);

    let total_progress = Arc::new(AtomicU64::new(0));
    let total_progress_clone = total_progress.clone();

    let result = download_piece(
        piece,
        &client,
        &header_data,
        temp_dir.path().to_path_buf(),
        cancel_token,
        move |bytes| {
            total_progress_clone.fetch_add(bytes, Ordering::Relaxed);
        },
    )
    .await;

    assert!(result.is_ok());
    // Total progress should equal the body size
    assert_eq!(total_progress.load(Ordering::Relaxed), 2048);
}
