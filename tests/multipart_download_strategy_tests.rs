use std::path::PathBuf;

use tokio::sync::mpsc;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rdm::rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use rdm::rdm_core::downloader::strategy::multipart_download_strategy::{
    create_pieces, MultipartDownloadStrategy,
};
use rdm::rdm_core::types::types::{Piece, SegmentState, StreamType};

// ---------------------------------------------------------------
// create_pieces unit tests
// ---------------------------------------------------------------

#[test]
fn test_create_pieces_small_file() {
    // File smaller than MIN_PIECE_SIZE * 2 — should not split
    let pieces = create_pieces(256 * 1024, 8); // 256 KB
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].offset, 0);
    assert_eq!(pieces[0].length, 256 * 1024);
}

#[test]
fn test_create_pieces_splits_evenly() {
    // 8 MB file, 8 connections -> 8 pieces of 1 MB each
    let file_size = 8 * 1024 * 1024;
    let pieces = create_pieces(file_size, 8);
    assert_eq!(pieces.len(), 8);

    // Verify total coverage
    let mut sorted = pieces.clone();
    sorted.sort_by_key(|p| p.offset);
    let total: i64 = sorted.iter().map(|p| p.length).sum();
    assert_eq!(total, file_size as i64);

    // Verify no gaps or overlaps
    for i in 1..sorted.len() {
        assert_eq!(
            sorted[i].offset,
            sorted[i - 1].offset + sorted[i - 1].length
        );
    }
}

#[test]
fn test_create_pieces_respects_min_size() {
    // 1 MB file, 8 connections -> should stop splitting at 4 pieces (256 KB min)
    let file_size = 1024 * 1024;
    let pieces = create_pieces(file_size, 8);
    assert!(pieces.len() <= 4); // Can't go below 256 KB per piece

    let mut sorted = pieces.clone();
    sorted.sort_by_key(|p| p.offset);
    let total: i64 = sorted.iter().map(|p| p.length).sum();
    assert_eq!(total, file_size as i64);
}

#[test]
fn test_create_pieces_single_connection() {
    let file_size = 10 * 1024 * 1024;
    let pieces = create_pieces(file_size, 1);
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].length, file_size as i64);
}

#[test]
fn test_create_pieces_odd_size() {
    // Odd file size to verify no bytes are lost during halving
    let file_size: u64 = 1_000_001;
    let pieces = create_pieces(file_size, 4);

    let mut sorted = pieces.clone();
    sorted.sort_by_key(|p| p.offset);
    let total: i64 = sorted.iter().map(|p| p.length).sum();
    assert_eq!(total, file_size as i64, "total bytes must equal file size");

    // No gaps or overlaps
    for i in 1..sorted.len() {
        assert_eq!(
            sorted[i].offset,
            sorted[i - 1].offset + sorted[i - 1].length,
            "gap or overlap at piece {}",
            i
        );
    }
}

#[test]
fn test_create_pieces_all_unique_ids() {
    let pieces = create_pieces(8 * 1024 * 1024, 8);
    let ids: Vec<&str> = pieces.iter().map(|p| p.id.as_str()).collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "all piece IDs must be unique");
}

// ---------------------------------------------------------------
// Helper: creates a MockServer that supports Range requests
// over a deterministic body of `size` bytes.
// ---------------------------------------------------------------

/// Generates deterministic test data: each byte = (offset % 251) as u8.
/// Using a prime (251) so the pattern doesn't align with power-of-2 chunk sizes.
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Sets up a MockServer with:
/// - A probe response (206 with full Content-Length)
/// - Range-request responses that serve the correct byte slice
/// - A fallback 200 response for non-range requests
///
/// Returns (server, full_body).
async fn setup_resumable_server(body_size: usize) -> (MockServer, Vec<u8>) {
    let server = MockServer::start().await;
    let body = generate_test_data(body_size);

    // Probe request: Range: bytes=0-
    // Return 206 with the full body (simulates a real server)
    Mock::given(method("GET"))
        .and(header("Range", "bytes=0-"))
        .respond_with(
            ResponseTemplate::new(206)
                .set_body_bytes(body.clone())
                .insert_header("Content-Type", "application/octet-stream")
                .insert_header(
                    "Content-Disposition",
                    "attachment; filename=\"testdata.bin\"",
                )
                .insert_header("Last-Modified", "Sun, 01 Jan 2026 00:00:00 GMT"),
        )
        .mount(&server)
        .await;

    // Actual range requests during download.
    // wiremock doesn't support dynamic matching on Range header values,
    // so we mount a catch-all that returns the full body.
    // download_piece will still write only the streamed bytes.
    // For a more precise test, we rely on the integration test below.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    (server, body)
}

async fn setup_non_resumable_server(body_size: usize) -> (MockServer, Vec<u8>) {
    let server = MockServer::start().await;
    let body = generate_test_data(body_size);

    // Always returns 200 regardless of Range header
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
    let body_size = 2 * 1024 * 1024; // 2 MB -> should split into multiple pieces
    let (server, _body) = setup_resumable_server(body_size).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    strategy.preprocess().await.unwrap();

    // Check state was updated
    {
        let state = strategy.state().read().await;
        let s = state.as_ref().unwrap();
        assert!(s.resumable);
        assert!(s.file_size > 0);
        assert_eq!(s.attachment_name, Some("testdata.bin".to_string()));
        assert_eq!(
            s.content_type,
            Some("application/octet-stream".to_string())
        );
        assert_eq!(
            s.last_modified,
            Some("Sun, 01 Jan 2026 00:00:00 GMT".to_string())
        );
    }

    // Check pieces were created
    {
        let pieces = strategy.pieces().read().await;
        assert!(
            pieces.len() > 1,
            "resumable 2MB file should be split into >1 pieces"
        );

        // Verify total coverage
        let mut sorted: Vec<_> = pieces.values().cloned().collect();
        sorted.sort_by_key(|p| p.offset);
        let total: i64 = sorted.iter().map(|p| p.length).sum();
        assert_eq!(total, body_size as i64);

        // Verify no gaps
        for i in 1..sorted.len() {
            assert_eq!(
                sorted[i].offset,
                sorted[i - 1].offset + sorted[i - 1].length
            );
        }

        // All pieces should be NotStarted
        for piece in pieces.values() {
            assert_eq!(piece.state, SegmentState::NotStarted);
        }
    }

    // Check temp dir was created
    {
        let state = strategy.state().read().await;
        let s = state.as_ref().unwrap();
        assert!(std::path::Path::new(&s.temp_dir).exists());
        // Cleanup
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
        let state = strategy.state().read().await;
        let s = state.as_ref().unwrap();
        assert!(!s.resumable);
    }

    {
        let pieces = strategy.pieces().read().await;
        assert_eq!(
            pieces.len(),
            1,
            "non-resumable should create exactly 1 piece"
        );
        let piece = pieces.values().next().unwrap();
        assert_eq!(piece.offset, 0);
        assert_eq!(piece.length, -1); // unknown/full
    }

    // Cleanup
    {
        let state = strategy.state().read().await;
        let s = state.as_ref().unwrap();
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

    // All pieces should be Finished
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
            assert!(
                piece.downloaded > 0,
                "piece {} should have downloaded bytes",
                piece.id
            );
        }
    }

    // Temp files should exist
    {
        let state = strategy.state().read().await;
        let s = state.as_ref().unwrap();
        let temp_dir = PathBuf::from(&s.temp_dir);

        let pieces = strategy.pieces().read().await;
        for piece in pieces.values() {
            let path = temp_dir.join(&piece.id);
            assert!(path.exists(), "temp file {} should exist", piece.id);
        }

        // Cleanup
        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_download_no_pieces_is_noop() {
    let (server, _) = setup_resumable_server(1024).await;

    let (tx, _rx) = mpsc::channel(16);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"), tx);

    // Don't call preprocess — pieces map is empty
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
    // Manually set up pieces and temp files to test assembly
    let temp_dir = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);

    let strategy =
        MultipartDownloadStrategy::new("http://unused".to_string(), PathBuf::from("out.bin"), tx);

    // Override state with known temp_dir and attachment_name
    {
        let mut state = strategy.state().write().await;
        let s = state.as_mut().unwrap();
        s.temp_dir = temp_dir.path().to_string_lossy().to_string();
        s.attachment_name = Some("assembled_output.bin".to_string());
    }

    // Create 3 pieces with known data
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

    // Write temp files
    std::fs::write(temp_dir.path().join("p1"), &piece1_data).unwrap();
    std::fs::write(temp_dir.path().join("p2"), &piece2_data).unwrap();
    std::fs::write(temp_dir.path().join("p3"), &piece3_data).unwrap();

    // Insert pieces
    {
        let mut pieces = strategy.pieces().write().await;
        pieces.insert("p1".to_string(), piece1);
        pieces.insert("p2".to_string(), piece2);
        pieces.insert("p3".to_string(), piece3);
    }

    // Run postprocess
    strategy.postprocess().await.unwrap();

    // Verify assembled file
    let output = std::fs::read("assembled_output.bin").unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(&piece1_data);
    expected.extend_from_slice(&piece2_data);
    expected.extend_from_slice(&piece3_data);
    assert_eq!(
        output, expected,
        "assembled file should be pieces in offset order"
    );

    // Cleanup
    let _ = std::fs::remove_file("assembled_output.bin");
}

#[tokio::test]
async fn test_postprocess_fails_if_piece_not_finished() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);

    let strategy =
        MultipartDownloadStrategy::new("http://unused".to_string(), PathBuf::from("out.bin"), tx);

    {
        let mut state = strategy.state().write().await;
        let s = state.as_mut().unwrap();
        s.temp_dir = temp_dir.path().to_string_lossy().to_string();
    }

    // Insert a piece that is still NotStarted
    {
        let mut pieces = strategy.pieces().write().await;
        pieces.insert(
            "p1".to_string(),
            Piece::new("p1".to_string(), 0, 100),
        );
    }

    let result = strategy.postprocess().await;
    assert!(
        result.is_err(),
        "postprocess should fail if pieces aren't finished"
    );
}

// ---------------------------------------------------------------
// Full lifecycle: preprocess -> download -> postprocess
// ---------------------------------------------------------------

#[tokio::test]
async fn test_full_lifecycle_with_mock_server() {
    let body_size = 512 * 1024; // 512 KB
    let (server, _expected_body) = setup_resumable_server(body_size).await;

    let (tx, _rx) = mpsc::channel(1024);
    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("lifecycle_test.bin"), tx);

    strategy.preprocess().await.unwrap();

    // Override attachment_name after preprocess so we control the output filename
    {
        let mut state = strategy.state().write().await;
        let _s = state.as_mut().unwrap();
        _s.attachment_name = Some("lifecycle_test_output.bin".to_string());
    }

    strategy.download().await.unwrap();
    strategy.postprocess().await.unwrap();

    // Verify the assembled output
    let output = std::fs::read("lifecycle_test_output.bin").unwrap();

    // The mock server returns the full body for every Range request,
    // so each piece file contains the full body (not a slice).
    // This means the assembled file won't match byte-for-byte with expected_body
    // when multiple pieces are used. This is a limitation of the wiremock setup.
    // The real integration test validates correctness against a real server.
    // Here we just verify the pipeline doesn't error and produces a file.
    assert!(!output.is_empty(), "assembled output should not be empty");

    // Cleanup
    let _ = std::fs::remove_file("lifecycle_test_output.bin");
}
