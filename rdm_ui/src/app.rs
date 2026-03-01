use dioxus::prelude::*;

use crate::api::{
    cancel_download, subscribe_progress, trigger_download, DownloadRequest, ProgressSnapshot,
    VideoItem,
};
use crate::styles::APP_CSS;

// ---------------------------------------------------------------------------
// App state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum View {
    FilePicker,
    Progress { download_id: String },
}

// ---------------------------------------------------------------------------
// Root component
// ---------------------------------------------------------------------------

#[component]
pub fn App(video: VideoItem) -> Element {
    let view = use_signal(|| View::FilePicker);

    rsx! {
        style { "{APP_CSS}" }
        match view() {
            View::FilePicker => rsx! {
                FilePickerView { video: video.clone(), view }
            },
            View::Progress { download_id } => rsx! {
                ProgressView {
                    download_id: download_id.clone(),
                    title: video.text.clone(),
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// View 1 — File picker
// ---------------------------------------------------------------------------

#[component]
fn FilePickerView(video: VideoItem, mut view: Signal<View>) -> Element {
    let default_filename = derive_filename(&video.text, &video.url, video.info.as_str());
    let default_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let default_path = default_dir.join(&default_filename);

    let mut output_path = use_signal(|| default_path.to_string_lossy().to_string());
    let mut error_msg   = use_signal(|| String::new());
    let mut downloading = use_signal(|| false);

    let video_clone = video.clone();

    rsx! {
        div { class: "view",

            // ── Header ──────────────────────────────────────────────────────
            div { class: "header",
                div { class: "header-icon header-icon--blue", "↓" }
                div { class: "header-text",
                    div { class: "header-title",   "Save Download" }
                    div { class: "header-subtitle", "{video.text}" }
                }
            }

            div { class: "divider divider--top" }

            // ── Source URL ───────────────────────────────────────────────────
            div { class: "field",
                div { class: "field-label", "Source URL" }
                div { class: "field-value", "{video.url}" }
            }

            // ── Save location ────────────────────────────────────────────────
            div { class: "field",
                div { class: "field-label", "Save to" }
                div { class: "path-row",
                    input {
                        r#type: "text",
                        class: "path-input",
                        value: "{output_path}",
                        oninput: move |e| output_path.set(e.value()),
                    }
                    button {
                        class: "btn btn--browse",
                        onclick: move |_| {
                            let current      = output_path();
                            let current_path = std::path::PathBuf::from(&current);
                            let start_dir    = current_path.parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            let fname = current_path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("download")
                                .to_string();

                            if let Some(path) = rfd::FileDialog::new()
                                .set_directory(&start_dir)
                                .set_file_name(&fname)
                                .save_file()
                            {
                                output_path.set(path.to_string_lossy().to_string());
                            }
                        },
                        "Browse…"
                    }
                }
            }

            // ── Error ────────────────────────────────────────────────────────
            if !error_msg().is_empty() {
                div { class: "error-banner", "{error_msg}" }
            }

            div { class: "spacer" }
            div { class: "divider divider--bottom" }

            // ── Buttons ──────────────────────────────────────────────────────
            div { class: "btn-row",
                button {
                    class: "btn btn--cancel",
                    onclick: move |_| dioxus::desktop::window().close(),
                    "Cancel"
                }
                button {
                    class: "btn btn--primary",
                    disabled: downloading(),
                    onclick: {
                        let video_for_download = video_clone.clone();
                        move |_| {
                            let path = output_path();
                            if path.trim().is_empty() {
                                error_msg.set("Please choose a save location.".to_string());
                                return;
                            }
                            error_msg.set(String::new());
                            downloading.set(true);

                            let req = DownloadRequest {
                                id:              video_for_download.id.clone(),
                                url:             video_for_download.url.clone(),
                                title:           video_for_download.text.clone(),
                                output_path:     path.clone(),
                                cookie:          video_for_download.cookie.clone(),
                                request_headers: video_for_download.request_headers.clone(),
                                user_agent:      video_for_download.user_agent.clone(),
                                referer:         video_for_download.referer.clone(),
                                info:            video_for_download.info.clone(),
                            };

                            spawn(async move {
                                match trigger_download(&req).await {
                                    Ok(resp) => {
                                        view.set(View::Progress { download_id: resp.id });
                                    }
                                    Err(e) => {
                                        error_msg.set(format!("Failed to start download: {}", e));
                                        downloading.set(false);
                                    }
                                }
                            });
                        }
                    },
                    if downloading() { "Starting…" } else { "Download" }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// View 2 — Progress
// ---------------------------------------------------------------------------

#[component]
fn ProgressView(download_id: String, title: String) -> Element {
    let mut snapshot = use_signal(|| ProgressSnapshot {
        total_bytes_downloaded: 0,
        total_bytes: 0,
        speed: 0.0,
        eta_secs: 0.0,
        done: false,
    });
    let mut error_msg = use_signal(|| String::new());

    let id_for_sse = download_id.clone();
    use_effect(move || {
        let id = id_for_sse.clone();
        spawn(async move {
            if let Err(e) = subscribe_progress(&id, move |snap| snapshot.set(snap)).await {
                error_msg.set(format!("Progress stream error: {}", e));
            }
        });
    });

    let snap          = snapshot();
    let pct           = if snap.total_bytes > 0 {
        (snap.total_bytes_downloaded as f64 / snap.total_bytes as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let speed_mb      = snap.speed / (1024.0 * 1024.0);
    let downloaded_mb = snap.total_bytes_downloaded as f64 / (1024.0 * 1024.0);
    let total_mb      = snap.total_bytes as f64 / (1024.0 * 1024.0);
    let is_done       = snap.done;

    let eta_str = if is_done {
        "Complete".to_string()
    } else if snap.eta_secs > 0.0 {
        format_eta(snap.eta_secs)
    } else {
        "Calculating…".to_string()
    };

    let bar_width = format!("{:.2}%", pct);

    rsx! {
        div { class: "view",

            // ── Header ──────────────────────────────────────────────────────
            div { class: "header",
                div {
                    class: if is_done { "header-icon header-icon--green" } else { "header-icon header-icon--blue" },
                    if is_done { "✓" } else { "↓" }
                }
                div { class: "header-text",
                    div { class: "header-title",
                        if is_done { "Download Complete" } else { "Downloading…" }
                    }
                    div { class: "header-subtitle", "{title}" }
                }
            }

            div { class: "divider divider--top" }

            // ── Percentage + bar ─────────────────────────────────────────────
            div { style: "flex-shrink: 0;",
                div { class: "pct-row",
                    span { class: "pct-hero", "{pct:.1}%" }
                    span { class: "pct-bytes",
                        if snap.total_bytes > 0 {
                            "{downloaded_mb:.1} / {total_mb:.1} MB"
                        } else {
                            "{downloaded_mb:.1} MB downloaded"
                        }
                    }
                }
                div { class: "bar-track",
                    div {
                        class: if is_done { "bar-fill bar-fill--green" } else { "bar-fill bar-fill--blue" },
                        style: "width: {bar_width};",
                    }
                }
            }

            // ── Stat cards ───────────────────────────────────────────────────
            div { class: "stats-row",
                div { class: "stat-card",
                    div { class: "stat-label", "Speed" }
                    div { class: "stat-value",
                        if is_done { "—" } else { "{speed_mb:.2} MB/s" }
                    }
                }
                div { class: "stat-card",
                    div { class: "stat-label", "ETA" }
                    div { class: "stat-value", "{eta_str}" }
                }
            }

            // ── Error ────────────────────────────────────────────────────────
            if !error_msg().is_empty() {
                div { class: "error-banner", style: "margin-top: 14px;", "{error_msg}" }
            }

            div { class: "spacer" }
            div { class: "divider divider--bottom" }

            // ── Button ───────────────────────────────────────────────────────
            div { class: "btn-row",
                if is_done {
                    button {
                        class: "btn btn--success",
                        onclick: move |_| dioxus::desktop::window().close(),
                        "Close"
                    }
                } else {
                    button {
                        class: "btn btn--danger",
                        onclick: {
                            let id = download_id.clone();
                            move |_| {
                                let id = id.clone();
                                spawn(async move {
                                    let _ = cancel_download(&id).await;
                                    dioxus::desktop::window().close();
                                });
                            }
                        },
                        "Cancel"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn derive_filename(title: &str, url: &str, mime: &str) -> String {
    let base = if !title.is_empty() {
        title
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
            .collect::<String>()
            .trim()
            .to_string()
    } else {
        url.rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("download")
            .to_string()
    };

    let ext = ext_from_mime(mime)
        .or_else(|| ext_from_url(url))
        .unwrap_or("mp4");

    if base.ends_with(&format!(".{}", ext)) { base } else { format!("{}.{}", base, ext) }
}

fn ext_from_mime(mime: &str) -> Option<&'static str> {
    match mime {
        m if m.contains("mp4")  => Some("mp4"),
        m if m.contains("webm") => Some("webm"),
        m if m.contains("mkv")  => Some("mkv"),
        m if m.contains("avi")  => Some("avi"),
        m if m.contains("mov")  => Some("mov"),
        m if m.contains("mp3")  => Some("mp3"),
        m if m.contains("ogg")  => Some("ogg"),
        m if m.contains("flac") => Some("flac"),
        m if m.contains("wav")  => Some("wav"),
        m if m.contains("m4v")  => Some("m4v"),
        _ => None,
    }
}

fn ext_from_url(url: &str) -> Option<&'static str> {
    let path = url.split('?').next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    if let Some(dot_pos) = last.rfind('.') {
        match &last[dot_pos + 1..] {
            "mp4"  => Some("mp4"),
            "webm" => Some("webm"),
            "mkv"  => Some("mkv"),
            "avi"  => Some("avi"),
            "mov"  => Some("mov"),
            "mp3"  => Some("mp3"),
            "ogg"  => Some("ogg"),
            "flac" => Some("flac"),
            "wav"  => Some("wav"),
            "m4v"  => Some("m4v"),
            "m3u8" => Some("m3u8"),
            _      => None,
        }
    } else {
        None
    }
}

fn format_eta(secs: f64) -> String {
    let s = secs as u64;
    if s >= 3600 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}s", s)
    }
}
