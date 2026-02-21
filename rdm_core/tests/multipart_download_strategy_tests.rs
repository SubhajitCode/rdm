use std::path::PathBuf;

use tokio::sync::mpsc;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use rdm_core::types::types::{Piece, SegmentState, StreamType};

/// Generates deterministic test data: each byte = (offset % 251) as u8.
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

async fn setup_resumable_server(body_size: usize) -> (MockServer, Vec<u8>) {
    let server = MockServer::start().await;
    let body = generate_test_data(body_size);

    Mock::given(method("GET"))
        .and(header("Range", "bytes=0-0"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(vec![0u8; 1])
                .insert_header(
                    "Content-Range",
                    format!("bytes 0-0/{}", body.len()),
                )
                .insert_header("Content-Type", "application/octet-stream")
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"test_data.bin\"",
                )
                .insert_header("Last-Modified", "Sun, 01 Jan 2026 00:00:00 GMT"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    (server, body)
}

async fn setup_non_resumable_server(body_size: usize) -> (MockServer, Vec<u8>) {
    let server = MockServer::start().await;
    let body = generate_test_data(body_size);

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("Content-Type", "application/octet-stream"),
        )
        .mount(&server)
        .await;

    (server, body)
}

// ---------------------------------------------------------------
// preprocess tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_preprocess_resumable_creates_multiple_pieces() {
    let body_size = 2 * 1024 * 1024;
    let (server, _body) = setup_resumable_server(body_size).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    strategy.preprocess().await.unwrap();

    {
        let state_lock = strategy.state().write().await;
        let s = &*state_lock;
        assert!(s.resumable);
        assert!(s.file_size > 0);
        assert_eq!(s.attachment_name, Some("test_data.bin".to_string()));
        assert_eq!(s.content_type, Some("application/octet-stream".to_string()));
        assert_eq!(s.last_modified, Some("Sun, 01 Jan 2026 00:00:00 GMT".to_string()));
    }

    {
        let pieces = strategy.pieces().read().await;
        assert!(pieces.len() > 1, "resumable 2MB file should be split into >1 pieces");

        let mut sorted: Vec<_> = pieces.values().cloned().collect();
        sorted.sort_by_key(|p| p.offset);
        let total: i64 = sorted.iter().map(|p| p.length).sum();
        assert_eq!(total, body_size as i64);

        for i in 1..sorted.len() {
            assert_eq!(sorted[i].offset, sorted[i - 1].offset + sorted[i - 1].length);
        }

        for piece in pieces.values() {
            assert_eq!(piece.state, SegmentState::NotStarted);
        }
    }

    {
        let state_lock = strategy.state().write().await;
        let s = &*state_lock;
        assert!(std::path::Path::new(&s.temp_dir).exists());
        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_preprocess_non_resumable_creates_single_piece() {
    let body_size = 2 * 1024 * 1024;
    let (server, _body) = setup_non_resumable_server(body_size).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    strategy.preprocess().await.unwrap();

    {
        let state_lock = strategy.state().write().await;
        let s = &*state_lock;
        assert!(!s.resumable);
    }

    {
        let pieces = strategy.pieces().read().await;
        assert_eq!(pieces.len(), 1, "non-resumable should create exactly 1 piece");
        let piece = pieces.values().next().unwrap();
        assert_eq!(piece.offset, 0);
        assert_eq!(piece.length, -1);
    }

    {
        let state_lock = strategy.state().write().await;
        let s = &*state_lock;
        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_preprocess_invalid_url_returns_error() {
    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(
        "http://127.0.0.1:1/nonexistent".to_string(),
        PathBuf::from("out.bin"),
        tx,
    );

    let result = strategy.preprocess().await;
    assert!(result.is_err(), "probing an unreachable URL should fail");
}

// ---------------------------------------------------------------
// download tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_download_writes_all_pieces_to_temp_files() {
    let body_size = 2 * 1024 * 1024;
    let (server, _body) = setup_resumable_server(body_size).await;

    let (tx, _rx) = mpsc::channel(1024);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    strategy.preprocess().await.unwrap();
    strategy.download().await.unwrap();

    {
        let pieces = strategy.pieces().read().await;
        for piece in pieces.values() {
            assert_eq!(
                piece.state,
                SegmentState::Finished,
                "piece {} should be Finished, got {:?}",
                piece.id,
                piece.state
            );
            assert!(piece.downloaded > 0, "piece {} should have downloaded bytes", piece.id);
        }
    }

    {
        let state_lock = strategy.state().write().await;
        let s = &*state_lock;
        let temp_dir = std::path::PathBuf::from(&s.temp_dir);

        let pieces = strategy.pieces().read().await;
        for piece in pieces.values() {
            let path = temp_dir.join(&piece.id);
            assert!(path.exists(), "temp file {} should exist", piece.id);
        }

        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_download_no_pieces_is_noop() {
    let (server, _) = setup_resumable_server(1024).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    let result = strategy.download().await;
    assert!(result.is_ok(), "download with no pieces should be Ok");
}

// ---------------------------------------------------------------
// stop / cancellation tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_stop_cancels_token() {
    let (server, _) = setup_resumable_server(1024).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    assert!(!strategy.cancel_token().is_cancelled());
    strategy.stop().await.unwrap();
    assert!(strategy.cancel_token().is_cancelled());
}

#[tokio::test]
async fn test_pause_cancels_token() {
    let (server, _) = setup_resumable_server(1024).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    strategy.pause().await.unwrap();
    assert!(strategy.cancel_token().is_cancelled());
}

// ---------------------------------------------------------------
// postprocess tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_postprocess_assembles_pieces_in_order() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);

    let strategy = MultipartDownloadStrategy::new(
        "http://unused".to_string(),
        PathBuf::from("assembled_output.bin"),
        tx,
    );

    {
        let mut s = strategy.state().write().await;
        s.temp_dir = temp_dir.path().to_string_lossy().to_string();
    }

    let piece1_data = vec![0x11u8; 100];
    let piece2_data = vec![0x22u8; 200];
    let piece3_data = vec![0x33u8; 150];

    let piece1 = Piece {
        id: "p1".to_string(),
        offset: 0,
        length: 100,
        downloaded: 100,
        state: SegmentState::Finished,
        stream_type: StreamType::Primary,
    };
    let piece2 = Piece {
        id: "p2".to_string(),
        offset: 100,
        length: 200,
        downloaded: 200,
        state: SegmentState::Finished,
        stream_type: StreamType::Primary,
    };
    let piece3 = Piece {
        id: "p3".to_string(),
        offset: 300,
        length: 150,
        downloaded: 150,
        state: SegmentState::Finished,
        stream_type: StreamType::Primary,
    };

    std::fs::write(temp_dir.path().join("p1"), &piece1_data).unwrap();
    std::fs::write(temp_dir.path().join("p2"), &piece2_data).unwrap();
    std::fs::write(temp_dir.path().join("p3"), &piece3_data).unwrap();

    {
        let mut pieces = strategy.pieces().write().await;
        pieces.insert("p1".to_string(), piece1);
        pieces.insert("p2".to_string(), piece2);
        pieces.insert("p3".to_string(), piece3);
    }

    strategy.postprocess().await.unwrap();

    let output = std::fs::read("assembled_output.bin").unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(&piece1_data);
    expected.extend_from_slice(&piece2_data);
    expected.extend_from_slice(&piece3_data);
    assert_eq!(output, expected, "assembled file should be pieces in offset order");

    let _ = std::fs::remove_file("assembled_output.bin");
}

#[tokio::test]
async fn test_postprocess_fails_if_piece_not_finished() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);

    let strategy = MultipartDownloadStrategy::new(
        "http://unused".to_string(),
        PathBuf::from("out.bin"),
        tx,
    );

    {
        let mut s = strategy.state().write().await;
        s.temp_dir = temp_dir.path().to_string_lossy().to_string();
    }

    {
        let mut pieces = strategy.pieces().write().await;
        pieces.insert("p1".to_string(), Piece::new("p1".to_string(), 0, 100));
    }

    let result = strategy.postprocess().await;
    assert!(result.is_err(), "postprocess should fail if pieces aren't finished");
}

// ---------------------------------------------------------------
// Full lifecycle: preprocess -> download -> postprocess
// ---------------------------------------------------------------

#[tokio::test]
async fn test_full_lifecycle_with_mock_server() {
    let body_size = 512 * 1024;
    let (server, _expected_body) = setup_resumable_server(body_size).await;

    let (tx, _rx) = mpsc::channel(1024);
    let strategy =
        MultipartDownloadStrategy::new(server.uri(), PathBuf::from("lifecycle_test.bin"), tx);

    strategy.preprocess().await.unwrap();
    strategy.download().await.unwrap();
    strategy.postprocess().await.unwrap();

    let output = std::fs::read("lifecycle_test.bin").unwrap();
    assert!(!output.is_empty(), "assembled output should not be empty");

    let _ = std::fs::remove_file("lifecycle_test.bin");
}
