use std::path::PathBuf;

use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rdm_core::downloader::strategy::download_strategy::DownloadStrategy;
use rdm_core::downloader::strategy::multipart_download_strategy::MultipartDownloadStrategy;
use rdm_core::types::types::{Segment, SegmentState, StreamType};

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
async fn test_preprocess_resumable_creates_multiple_segments() {
    let body_size = 2 * 1024 * 1024;
    let (server, _body) = setup_resumable_server(body_size).await;

    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"));

    strategy.preprocess().await.unwrap();

    {
        let state_lock = strategy.state().write().unwrap();
        let s = &*state_lock;
        assert!(s.resumable);
        assert!(s.file_size > 0);
        assert_eq!(s.attachment_name, Some("test_data.bin".to_string()));
        assert_eq!(s.content_type, Some("application/octet-stream".to_string()));
        assert_eq!(s.last_modified, Some("Sun, 01 Jan 2026 00:00:00 GMT".to_string()));
    }

    {
        let segments = strategy.segments().read().await;
        assert!(segments.len() > 1, "resumable 2MB file should be split into >1 segments");

        let mut sorted: Vec<_> = segments.values().cloned().collect();
        sorted.sort_by_key(|p| p.offset);
        let total: i64 = sorted.iter().map(|p| p.length).sum();
        assert_eq!(total, body_size as i64);

        for i in 1..sorted.len() {
            assert_eq!(sorted[i].offset, sorted[i - 1].offset + sorted[i - 1].length);
        }

        for segment in segments.values() {
            assert_eq!(segment.state, SegmentState::NotStarted);
        }
    }

    {
        let state_lock = strategy.state().write().unwrap();
        let s = &*state_lock;
        assert!(std::path::Path::new(&s.temp_dir).exists());
        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_preprocess_non_resumable_creates_single_segment() {
    let body_size = 2 * 1024 * 1024;
    let (server, _body) = setup_non_resumable_server(body_size).await;

    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"));

    strategy.preprocess().await.unwrap();

    {
        let state_lock = strategy.state().write().unwrap();
        let s = &*state_lock;
        assert!(!s.resumable);
    }

    {
        let segments = strategy.segments().read().await;
        assert_eq!(segments.len(), 1, "non-resumable should create exactly 1 segment");
        let segment = segments.values().next().unwrap();
        assert_eq!(segment.offset, 0);
        assert_eq!(segment.length, -1);
    }

    {
        let state_lock = strategy.state().write().unwrap();
        let s = &*state_lock;
        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_preprocess_invalid_url_returns_error() {
    let strategy = MultipartDownloadStrategy::new(
        "http://127.0.0.1:1/nonexistent".to_string(),
        PathBuf::from("out.bin"),
    );

    let result = strategy.preprocess().await;
    assert!(result.is_err(), "probing an unreachable URL should fail");
}

// ---------------------------------------------------------------
// download tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_download_writes_all_segments_to_temp_files() {
    let body_size = 2 * 1024 * 1024;
    let (server, _body) = setup_resumable_server(body_size).await;

    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"));

    strategy.preprocess().await.unwrap();
    strategy.download().await.unwrap();

    {
        let segments = strategy.segments().read().await;
        for segment in segments.values() {
            assert_eq!(
                segment.state,
                SegmentState::Finished,
                "segment {} should be Finished, got {:?}",
                segment.id,
                segment.state
            );
            assert!(segment.downloaded > 0, "segment {} should have downloaded bytes", segment.id);
        }
    }

    {
        let state_lock = strategy.state().write().unwrap();
        let s = &*state_lock;
        let temp_dir = std::path::PathBuf::from(&s.temp_dir);

        let segments = strategy.segments().read().await;
        for segment in segments.values() {
            let path = temp_dir.join(&segment.id);
            assert!(path.exists(), "temp file {} should exist", segment.id);
        }

        let _ = std::fs::remove_dir_all(&s.temp_dir);
    }
}

#[tokio::test]
async fn test_download_no_segments_is_noop() {
    let (server, _) = setup_resumable_server(1024).await;

    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"));

    let result = strategy.download().await;
    assert!(result.is_ok(), "download with no segments should be Ok");
}

// ---------------------------------------------------------------
// stop / cancellation tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_stop_cancels_token() {
    let (server, _) = setup_resumable_server(1024).await;

    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"));

    assert!(!strategy.cancel_token().is_cancelled());
    strategy.stop().await.unwrap();
    assert!(strategy.cancel_token().is_cancelled());
}

#[tokio::test]
async fn test_pause_cancels_token() {
    let (server, _) = setup_resumable_server(1024).await;

    let strategy = MultipartDownloadStrategy::new(server.uri(), PathBuf::from("out.bin"));

    strategy.pause().await.unwrap();
    assert!(strategy.cancel_token().is_cancelled());
}

// ---------------------------------------------------------------
// postprocess tests
// ---------------------------------------------------------------

#[tokio::test]
async fn test_postprocess_assembles_segments_in_order() {
    let temp_dir = tempfile::tempdir().unwrap();

    let strategy = MultipartDownloadStrategy::new(
        "http://unused".to_string(),
        PathBuf::from("assembled_output.bin"),
    );

    {
        let mut s = strategy.state().write().unwrap();
        s.temp_dir = temp_dir.path().to_string_lossy().to_string();
    }

    let segment1_data = vec![0x11u8; 100];
    let segment2_data = vec![0x22u8; 200];
    let segment3_data = vec![0x33u8; 150];

    let segment1 = Segment {
        id: "p1".to_string(),
        offset: 0,
        length: 100,
        downloaded: 100,
        state: SegmentState::Finished,
        stream_type: StreamType::Primary,
    };
    let segment2 = Segment {
        id: "p2".to_string(),
        offset: 100,
        length: 200,
        downloaded: 200,
        state: SegmentState::Finished,
        stream_type: StreamType::Primary,
    };
    let segment3 = Segment {
        id: "p3".to_string(),
        offset: 300,
        length: 150,
        downloaded: 150,
        state: SegmentState::Finished,
        stream_type: StreamType::Primary,
    };

    std::fs::write(temp_dir.path().join("p1"), &segment1_data).unwrap();
    std::fs::write(temp_dir.path().join("p2"), &segment2_data).unwrap();
    std::fs::write(temp_dir.path().join("p3"), &segment3_data).unwrap();

    {
        let mut segments = strategy.segments().write().await;
        segments.insert("p1".to_string(), segment1);
        segments.insert("p2".to_string(), segment2);
        segments.insert("p3".to_string(), segment3);
    }

    strategy.postprocess().await.unwrap();

    let output = std::fs::read("assembled_output.bin").unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(&segment1_data);
    expected.extend_from_slice(&segment2_data);
    expected.extend_from_slice(&segment3_data);
    assert_eq!(output, expected, "assembled file should be segments in offset order");

    let _ = std::fs::remove_file("assembled_output.bin");
}

#[tokio::test]
async fn test_postprocess_fails_if_segment_not_finished() {
    let temp_dir = tempfile::tempdir().unwrap();

    let strategy = MultipartDownloadStrategy::new(
        "http://unused".to_string(),
        PathBuf::from("out.bin"),
    );

    {
        let mut s = strategy.state().write().unwrap();
        s.temp_dir = temp_dir.path().to_string_lossy().to_string();
    }

    {
        let mut segments = strategy.segments().write().await;
        segments.insert("p1".to_string(), Segment::new("p1".to_string(), 0, 100));
    }

    let result = strategy.postprocess().await;
    assert!(result.is_err(), "postprocess should fail if segments aren't finished");
}

// ---------------------------------------------------------------
// Full lifecycle: preprocess -> download -> postprocess
// ---------------------------------------------------------------

#[tokio::test]
async fn test_full_lifecycle_with_mock_server() {
    let body_size = 512 * 1024;
    let (server, _expected_body) = setup_resumable_server(body_size).await;

    let strategy =
        MultipartDownloadStrategy::new(server.uri(), PathBuf::from("lifecycle_test.bin"));

    strategy.preprocess().await.unwrap();
    strategy.download().await.unwrap();
    strategy.postprocess().await.unwrap();

    let output = std::fs::read("lifecycle_test.bin").unwrap();
    assert!(!output.is_empty(), "assembled output should not be empty");

    let _ = std::fs::remove_file("lifecycle_test.bin");
}
