//! Path sanitizer — produces a safe, collision-free output path for a download.
//!
//! # What it does
//! 1. Resolves the download directory (env `RDM_DOWNLOAD_DIR` → `~/Downloads/rdm`).
//! 2. Sanitises the suggested filename:
//!    - Strips / replaces characters that are illegal on macOS, Linux or Windows.
//!    - Collapses runs of whitespace / underscores.
//!    - Prevents path traversal (dots-only segments, absolute paths, `..`).
//!    - Falls back to `"download"` if nothing usable remains.
//! 3. Preserves the file extension (up to 10 chars, alphanumeric only).
//! 4. Avoids collisions by appending `_2`, `_3`, … when the file already exists.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return a safe, collision-free [`PathBuf`] for saving a download.
///
/// # Arguments
/// * `suggested`    – The filename hint (e.g. tab title, `attachment_name`).
///                    May be empty, contain path separators, or be garbage.
/// * `url`          – The download URL, used as a fallback when `suggested` is
///                    unusable.
/// * `content_type` – Optional MIME type (e.g. `"video/mp4"`). Used to supply
///                    a proper extension when `suggested` carries none.
///
/// # Panics
/// Never panics — all error paths produce a reasonable fallback.
pub fn safe_output_path(suggested: &str, url: &str, content_type: Option<&str>) -> PathBuf {
    let dir = download_dir();
    let name = sanitise_filename(suggested, url, content_type);
    unique_path(dir, &name)
}

// ---------------------------------------------------------------------------
// Download directory
// ---------------------------------------------------------------------------

/// Returns the download directory, creating it if needed.
/// Priority: `$RDM_DOWNLOAD_DIR` → `~/Downloads/rdm`.
fn download_dir() -> PathBuf {
    let dir = if let Ok(env_dir) = std::env::var("RDM_DOWNLOAD_DIR") {
        PathBuf::from(env_dir)
    } else {
        dirs_next::download_dir()
            .unwrap_or_else(|| {
                dirs_next::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("Downloads")
            })
            .join("rdm")
    };

    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("[path] could not create download dir {:?}: {}", dir, e);
        }
    }

    dir
}

// ---------------------------------------------------------------------------
// Filename sanitisation
// ---------------------------------------------------------------------------

/// Characters that are always safe in a filename on macOS / Linux / Windows.
/// Anything outside this set is replaced with `_`.
/// Note: space is intentionally excluded — spaces are normalised to `_` so
/// that filenames never contain whitespace.
fn is_safe_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '-' | '_' | '.' | '(' | ')' | '[' | ']' | '+' | ',' | '@' | '~'
        )
}

/// Sanitise `suggested`, falling back to `url` if necessary.
/// Returns a filename **with** extension, e.g. `"My_Video_HD.mp4"`.
///
/// If neither `suggested` nor `url` carry an extension, `content_type` is
/// consulted to pick an appropriate one (e.g. `"video/mp4"` → `.mp4`).
fn sanitise_filename(suggested: &str, url: &str, content_type: Option<&str>) -> String {
    // Try the suggestion first; fall back to last URL path segment.
    let raw = if !suggested.trim().is_empty() {
        suggested.to_string()
    } else {
        filename_from_url(url)
    };

    // Strip any leading path components (prevents traversal).
    let raw = PathBuf::from(&raw)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or(raw.clone());

    // Split stem and extension before sanitising to preserve extension as-is.
    let (stem, ext) = split_stem_ext(&raw);

    // Sanitise stem: replace unsafe chars (including space) with `_`.
    let stem: String = stem
        .chars()
        .map(|c| if is_safe_char(c) { c } else { '_' })
        .collect();

    // Collapse consecutive underscores.
    let stem = collapse_runs(&stem);
    let stem = stem.trim_matches(|c| c == '_' || c == '.').to_string();

    // Guard against empty or dots-only stems.
    let stem = if stem.is_empty() || stem.chars().all(|c| c == '.') {
        "download".to_string()
    } else {
        stem
    };

    // Limit stem length.
    let stem = truncate_to_bytes(&stem, 180);

    // Sanitise extension (alphanumeric only, max 10 chars).
    let ext = sanitise_ext(&ext);

    // If no extension was found in the filename, try to derive one from the
    // MIME type or from the URL path segment.
    let ext = if ext.is_empty() {
        ext_from_content_type(content_type)
            .or_else(|| ext_from_url(url))
            .unwrap_or_default()
    } else {
        ext
    };

    if ext.is_empty() {
        stem
    } else {
        format!("{}.{}", stem, ext)
    }
}

/// Split a filename into `(stem, extension)`.
/// Extension is the part after the last `.`; empty if no dot or leading dot only.
fn split_stem_ext(name: &str) -> (String, String) {
    let p = PathBuf::from(name);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    (stem, ext)
}

/// Sanitise an extension: lowercase, alphanumeric only, max 10 chars.
fn sanitise_ext(ext: &str) -> String {
    let clean: String = ext
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(10)
        .collect::<String>()
        .to_lowercase();
    clean
}

/// Map a MIME type to a common file extension, e.g. `"video/mp4"` → `"mp4"`.
/// Returns `None` for unknown or missing types.
fn ext_from_content_type(content_type: Option<&str>) -> Option<String> {
    let mime = content_type?
        .split(';') // strip parameters like "; charset=utf-8"
        .next()?
        .trim()
        .to_lowercase();

    let ext = match mime.as_str() {
        // Video
        "video/mp4" | "video/x-m4v" => "mp4",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        "video/quicktime" => "mov",
        "video/x-ms-wmv" => "wmv",
        "video/3gpp" => "3gp",
        "video/x-flv" => "flv",
        "video/mpeg" => "mpg",
        // Audio
        "audio/mpeg" => "mp3",
        "audio/flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/aac" => "aac",
        "audio/x-m4a" | "audio/mp4" => "m4a",
        "audio/opus" => "opus",
        // Archives
        "application/zip" => "zip",
        "application/x-tar" => "tar",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-bzip2" => "bz2",
        "application/x-7z-compressed" => "7z",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-xz" => "xz",
        // Executables / packages
        "application/x-msdownload" | "application/octet-stream" if false => "exe", // too generic
        "application/x-ms-installer" | "application/x-msi" => "msi",
        "application/vnd.debian.binary-package" => "deb",
        "application/x-rpm" => "rpm",
        "application/x-apple-diskimage" => "dmg",
        "application/x-newton-compatible-pkg" => "pkg",
        // Documents
        "application/pdf" => "pdf",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        // Images
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => return None,
    };
    Some(ext.to_string())
}

/// Extract the extension from the URL path (strip query / fragment first).
/// Returns `None` when the URL path has no recognisable extension.
fn ext_from_url(url: &str) -> Option<String> {
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);
    let last_seg = url.rsplit('/').find(|s| !s.is_empty())?;
    let ext = PathBuf::from(last_seg)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())?;
    let ext = sanitise_ext(&ext);
    if ext.is_empty() {
        None
    } else {
        Some(ext)
    }
}

/// Collapse consecutive `_` characters to a single `_`.
/// (Space is no longer allowed in stems, so only `_` runs need collapsing.)
fn collapse_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last: Option<char> = None;
    for c in s.chars() {
        if c == '_' && last == Some('_') {
            continue; // skip duplicate underscore
        }
        out.push(c);
        last = Some(c);
    }
    out
}

/// Truncate `s` to at most `max_bytes` UTF-8 bytes, respecting char boundaries.
fn truncate_to_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Extract the last non-empty path segment from a URL (strip query / fragment).
fn filename_from_url(url: &str) -> String {
    // Strip query and fragment.
    let url = url.split('?').next().unwrap_or(url);
    let url = url.split('#').next().unwrap_or(url);
    url.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("download")
        .to_string()
}

// ---------------------------------------------------------------------------
// Collision avoidance
// ---------------------------------------------------------------------------

/// Return a path that does not exist yet, appending `_2`, `_3`, … as needed.
fn unique_path(dir: PathBuf, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }

    let (stem, ext) = split_stem_ext(name);
    for n in 2u32..=9999 {
        let new_name = if ext.is_empty() {
            format!("{}_{}", stem, n)
        } else {
            format!("{}_{}.{}", stem, n, ext)
        };
        let candidate = dir.join(&new_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    // Absolute last resort — use a UUID-based name.
    dir.join(format!("download_{}.bin", uuid_suffix()))
}

fn uuid_suffix() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    let mut h = DefaultHasher::new();
    SystemTime::now().hash(&mut h);
    format!("{:016x}", h.finish())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_name_preserved() {
        let (stem, ext) = split_stem_ext("My Video (HD).mp4");
        assert_eq!(stem, "My Video (HD)");
        assert_eq!(ext, "mp4");
    }

    #[test]
    fn traversal_stripped() {
        let name = sanitise_filename("../../etc/passwd", "http://example.com", None);
        // Path traversal segments stripped — only last component kept.
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
    }

    #[test]
    fn illegal_chars_replaced() {
        let name = sanitise_filename("hello:world<>?*.mp4", "http://x.com", None);
        assert!(!name.contains(':'));
        assert!(!name.contains('<'));
        assert!(!name.contains('*'));
        assert!(!name.contains('?'));
    }

    #[test]
    fn empty_suggestion_uses_url() {
        let name = sanitise_filename("", "https://cdn.example.com/path/video.mp4", None);
        assert!(name.starts_with("video"));
    }

    #[test]
    fn extension_sanitised() {
        let ext = sanitise_ext("MP4<>");
        assert_eq!(ext, "mp4");
    }

    #[test]
    fn collapse_underscores() {
        let s = collapse_runs("hello___world");
        assert_eq!(s, "hello_world");
    }

    // ----- spaces → underscores -----

    #[test]
    fn spaces_replaced_with_underscores() {
        let name = sanitise_filename("My Video HD.mp4", "http://x.com", None);
        assert!(
            !name.contains(' '),
            "filename should not contain spaces: {name}"
        );
        assert!(name.starts_with("My_Video_HD"), "unexpected stem: {name}");
        assert!(name.ends_with(".mp4"));
    }

    #[test]
    fn leading_trailing_spaces_trimmed() {
        let name = sanitise_filename("  file name  .zip", "http://x.com", None);
        assert!(!name.starts_with('_'));
        assert!(!name.contains(' '));
    }

    // ----- extension from MIME type -----

    #[test]
    fn ext_added_from_content_type_when_missing() {
        let name = sanitise_filename("My_Show_Episode_1", "http://x.com/ep1", Some("video/mp4"));
        assert_eq!(name, "My_Show_Episode_1.mp4");
    }

    #[test]
    fn ext_not_duplicated_when_already_present() {
        let name = sanitise_filename("movie.mkv", "http://x.com", Some("video/x-matroska"));
        assert_eq!(name, "movie.mkv");
    }

    #[test]
    fn ext_inferred_from_url_when_no_content_type() {
        let name = sanitise_filename("Unnamed Track", "https://cdn.example.com/track.flac", None);
        assert_eq!(name, "Unnamed_Track.flac");
    }

    #[test]
    fn ext_from_content_type_takes_precedence_over_url() {
        // The suggested name has no ext; content_type is specific; URL has a different ext.
        let name = sanitise_filename(
            "Video Title",
            "https://cdn.example.com/file.bin",
            Some("video/mp4"),
        );
        assert_eq!(name, "Video_Title.mp4");
    }

    #[test]
    fn ext_from_content_type_with_charset_param() {
        // MIME type with extra params: "; charset=utf-8" should still be parsed.
        let name = sanitise_filename(
            "doc",
            "http://x.com",
            Some("application/pdf; charset=utf-8"),
        );
        assert_eq!(name, "doc.pdf");
    }

    // ----- ext_from_content_type helper -----

    #[test]
    fn mime_video_mp4_maps_to_mp4() {
        assert_eq!(
            ext_from_content_type(Some("video/mp4")),
            Some("mp4".to_string())
        );
    }

    #[test]
    fn mime_audio_mpeg_maps_to_mp3() {
        assert_eq!(
            ext_from_content_type(Some("audio/mpeg")),
            Some("mp3".to_string())
        );
    }

    #[test]
    fn mime_unknown_returns_none() {
        assert_eq!(
            ext_from_content_type(Some("application/octet-stream")),
            None
        );
    }

    #[test]
    fn mime_none_returns_none() {
        assert_eq!(ext_from_content_type(None), None);
    }
}
