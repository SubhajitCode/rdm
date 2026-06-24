use std::path::PathBuf;

use crate::types::types::DownloadKind;

// ---------------------------------------------------------------------------
// Extension helpers (shared by both download strategies)
// ---------------------------------------------------------------------------

/// If `path` already has a file extension, return it unchanged.
/// Otherwise try to derive an extension from `attachment_name` (Content-
/// Disposition) or `content_type` (MIME type) and append it.
pub fn ensure_extension(
    path: String,
    attachment_name: Option<&str>,
    content_type: Option<&str>,
) -> String {
    let pb = PathBuf::from(&path);
    if pb.extension().is_some() {
        return path;
    }

    let ext = attachment_name
        .and_then(|n| {
            PathBuf::from(n)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
        })
        .or_else(|| ext_from_mime(content_type));

    match ext {
        Some(e) if !e.is_empty() => format!("{}.{}", path, e.to_lowercase()),
        _ => path,
    }
}

pub fn ext_from_mime(content_type: Option<&str>) -> Option<String> {
    let mime = content_type?.split(';').next()?.trim().to_lowercase();

    let ext = match mime.as_str() {
        "application/vnd.apple.mpegurl" | "application/x-mpegurl" => "m3u8",
        "application/dash+xml" => "mpd",
        "video/mp2t" => "ts",
        "video/mp4" | "video/x-m4v" => "mp4",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        "video/quicktime" => "mov",
        "video/x-ms-wmv" => "wmv",
        "video/3gpp" => "3gp",
        "video/x-flv" => "flv",
        "video/mpeg" => "mpg",
        "audio/mpeg" => "mp3",
        "audio/flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/aac" => "aac",
        "audio/x-m4a" | "audio/mp4" => "m4a",
        "audio/opus" => "opus",
        "application/zip" => "zip",
        "application/x-tar" => "tar",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-bzip2" => "bz2",
        "application/x-7z-compressed" => "7z",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/pdf" => "pdf",
        "application/x-msdownload" => "exe",
        "application/x-ms-installer" | "application/x-msi" => "msi",
        "application/vnd.debian.binary-package" => "deb",
        "application/x-rpm" => "rpm",
        "application/x-apple-diskimage" => "dmg",
        _ => return None,
    };
    Some(ext.to_string())
}

pub fn detect_download_kind(url: &str, content_type: Option<&str>) -> DownloadKind {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".m3u8") {
        return DownloadKind::Hls;
    }
    if path.ends_with(".mpd") {
        return DownloadKind::Dash;
    }

    let mime = content_type
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());

    match mime.as_deref() {
        Some("application/vnd.apple.mpegurl" | "application/x-mpegurl") => DownloadKind::Hls,
        Some("application/dash+xml") => DownloadKind::Dash,
        _ => DownloadKind::Direct,
    }
}

pub fn replace_extension(path: &str, ext: &str) -> String {
    let mut pb = PathBuf::from(path);
    pb.set_extension(ext);
    pb.to_string_lossy().to_string()
}
