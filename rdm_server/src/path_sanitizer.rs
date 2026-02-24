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
/// * `suggested` – The filename hint (e.g. tab title, `attachment_name`).
///                 May be empty, contain path separators, or be garbage.
/// * `url`       – The download URL, used as a fallback when `suggested` is
///                 unusable.
///
/// # Panics
/// Never panics — all error paths produce a reasonable fallback.
pub fn safe_output_path(suggested: &str, url: &str) -> PathBuf {
    let dir = download_dir();
    let name = sanitise_filename(suggested, url);
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
fn is_safe_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '-' | '_' | '.' | ' ' | '(' | ')' | '[' | ']' | '+' | ',' | '@' | '~'
        )
}

/// Sanitise `suggested`, falling back to `url` if necessary.
/// Returns a filename **with** extension, e.g. `"My Video (HD).mp4"`.
fn sanitise_filename(suggested: &str, url: &str) -> String {
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

    // Sanitise stem.
    let stem: String = stem
        .chars()
        .map(|c| if is_safe_char(c) { c } else { '_' })
        .collect();

    // Collapse consecutive underscores / spaces.
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

/// Collapse runs of `_` or space to a single `_` or space respectively.
fn collapse_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last: Option<char> = None;
    for c in s.chars() {
        match (last, c) {
            (Some('_'), '_') | (Some(' '), ' ') => {} // skip duplicate
            _ => {
                out.push(c);
                last = Some(c);
            }
        }
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
        let name = sanitise_filename("../../etc/passwd", "http://example.com");
        // Path traversal segments stripped — only last component kept.
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
    }

    #[test]
    fn illegal_chars_replaced() {
        let name = sanitise_filename("hello:world<>?*.mp4", "http://x.com");
        assert!(!name.contains(':'));
        assert!(!name.contains('<'));
        assert!(!name.contains('*'));
        assert!(!name.contains('?'));
    }

    #[test]
    fn empty_suggestion_uses_url() {
        let name = sanitise_filename("", "https://cdn.example.com/path/video.mp4");
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
}
