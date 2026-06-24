use std::path::PathBuf;

use aes::Aes128;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use rdm_core::downloader::http_downloader::HttpDownloader;
use rdm_core::types::types::DownloaderState;

type Aes128CbcEnc = cbc::Encryptor<Aes128>;

#[tokio::test]
async fn test_http_downloader_hls_master_playlist_downloads_stream() {
    let server = MockServer::start().await;

    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=64000\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nhigh.m3u8\n";
    let media = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-ENDLIST\n#EXTINF:2.0,\nseg1.ts\n#EXTINF:2.0,\nseg2.ts\n";
    let expected = [b"segment-one".as_slice(), b"segment-two".as_slice()].concat();

    mount_text(&server, "/master.m3u8", master, "application/vnd.apple.mpegurl").await;
    mount_text(&server, "/low.m3u8", media, "application/vnd.apple.mpegurl").await;
    mount_text(&server, "/high.m3u8", media, "application/vnd.apple.mpegurl").await;
    mount_bytes(&server, "/seg1.ts", b"segment-one".to_vec(), "video/mp2t").await;
    mount_bytes(&server, "/seg2.ts", b"segment-two".to_vec(), "video/mp2t").await;

    let requested_output = format!("test_hls_output_{}.m3u8", uuid::Uuid::new_v4());
    let expected_output = requested_output.replace(".m3u8", ".ts");
    let state = DownloaderState::new(
        format!("{}/master.m3u8", server.uri()),
        PathBuf::from(&requested_output),
    );
    let mut downloader = HttpDownloader::new(state, 4);
    downloader.download().await.unwrap();

    let output = std::fs::read(&expected_output).unwrap();
    assert_eq!(output, expected);
    assert!(!PathBuf::from(&requested_output).exists(), "manifest extension should be replaced");

    let _ = std::fs::remove_file(expected_output);
}

#[tokio::test]
async fn test_http_downloader_hls_aes128_decrypts_segments() {
    let server = MockServer::start().await;

    let key = b"0123456789abcdef";
    let media = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-KEY:METHOD=AES-128,URI=\"enc.key\"\n#EXT-X-ENDLIST\n#EXTINF:2.0,\nseg1.ts\n#EXTINF:2.0,\nseg2.ts\n";

    mount_text(&server, "/secure.m3u8", media, "application/vnd.apple.mpegurl").await;
    mount_bytes(&server, "/enc.key", key.to_vec(), "application/octet-stream").await;
    mount_bytes(
        &server,
        "/seg1.ts",
        encrypt_hls_segment(b"secret-one".to_vec(), key, hls_iv(1)),
        "video/mp2t",
    )
    .await;
    mount_bytes(
        &server,
        "/seg2.ts",
        encrypt_hls_segment(b"secret-two".to_vec(), key, hls_iv(2)),
        "video/mp2t",
    )
    .await;

    let output_name = format!("test_hls_enc_{}.ts", uuid::Uuid::new_v4());
    let state = DownloaderState::new(
        format!("{}/secure.m3u8", server.uri()),
        PathBuf::from(&output_name),
    );
    let mut downloader = HttpDownloader::new(state, 2);
    downloader.download().await.unwrap();

    let output = std::fs::read(&output_name).unwrap();
    assert_eq!(output, [b"secret-one".as_slice(), b"secret-two".as_slice()].concat());

    let _ = std::fs::remove_file(output_name);
}

#[tokio::test]
async fn test_http_downloader_dash_segment_template_downloads_stream() {
    let server = MockServer::start().await;

    let mpd = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT4S" minBufferTime="PT1S">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <Representation id="v1" bandwidth="128000">
        <SegmentTemplate timescale="1" duration="2" startNumber="1" initialization="init.mp4" media="seg-$Number$.m4s" />
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    let init = b"dash-init".to_vec();
    let seg1 = b"dash-seg-one".to_vec();
    let seg2 = b"dash-seg-two".to_vec();

    mount_text(&server, "/manifest.mpd", mpd, "application/dash+xml").await;
    mount_bytes(&server, "/init.mp4", init.clone(), "video/mp4").await;
    mount_bytes(&server, "/seg-1.m4s", seg1.clone(), "video/iso.segment").await;
    mount_bytes(&server, "/seg-2.m4s", seg2.clone(), "video/iso.segment").await;

    let requested_output = format!("test_dash_output_{}.mpd", uuid::Uuid::new_v4());
    let expected_output = requested_output.replace(".mpd", ".mp4");
    let state = DownloaderState::new(
        format!("{}/manifest.mpd", server.uri()),
        PathBuf::from(&requested_output),
    );
    let mut downloader = HttpDownloader::new(state, 3);
    downloader.download().await.unwrap();

    let output = std::fs::read(&expected_output).unwrap();
    assert_eq!(output, [init, seg1, seg2].concat());
    assert!(!PathBuf::from(&requested_output).exists(), "manifest extension should be replaced");

    let _ = std::fs::remove_file(expected_output);
}

async fn mount_text(server: &MockServer, route: &str, body: &str, content_type: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", content_type)
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn mount_bytes(server: &MockServer, route: &str, body: Vec<u8>, content_type: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", content_type)
                .set_body_bytes(body),
        )
        .mount(server)
        .await;
}

fn hls_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

fn encrypt_hls_segment(plaintext: Vec<u8>, key: &[u8], iv: [u8; 16]) -> Vec<u8> {
    Aes128CbcEnc::new_from_slices(key, &iv)
        .unwrap()
        .encrypt_padded_vec_mut::<Pkcs7>(&plaintext)
}
