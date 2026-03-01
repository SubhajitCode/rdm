use dioxus::prelude::*;

use crate::api::{
    cancel_download, subscribe_progress, trigger_download, DownloadRequest, ProgressSnapshot,
    VideoItem,
};

// ---------------------------------------------------------------------------
// App state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum View {
    /// File-picker view: user chooses where to save.
    FilePicker,
    /// Progress view: download is running / done.
    Progress { download_id: String },
}

// ---------------------------------------------------------------------------
// Root component
// ---------------------------------------------------------------------------

#[component]
pub fn App(video: VideoItem) -> Element {
    let view = use_signal(|| View::FilePicker);

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

// ---------------------------------------------------------------------------
// View 1 — File picker
// ---------------------------------------------------------------------------

#[component]
fn FilePickerView(video: VideoItem, mut view: Signal<View>) -> Element {
    // Derive a sensible default filename from the video title + mime type.
    let default_filename = derive_filename(&video.text, &video.url, video.info.as_str());
    let default_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let default_path = default_dir.join(&default_filename);

    let mut output_path = use_signal(|| default_path.to_string_lossy().to_string());
    let mut error_msg = use_signal(|| String::new());
    let mut downloading = use_signal(|| false);

    // Clone video for the async closures below.
    let video_clone = video.clone();

    rsx! {
        div {
            style: "font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #1e1e2e; color: #cdd6f4; min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: 24px;",
            div {
                style: "background: #313244; border-radius: 12px; padding: 28px 32px; width: 520px; box-shadow: 0 8px 32px rgba(0,0,0,0.4);",

                // Header
                div {
                    style: "display: flex; align-items: center; gap: 12px; margin-bottom: 20px;",
                    div {
                        style: "width: 36px; height: 36px; background: #89b4fa; border-radius: 8px; display: flex; align-items: center; justify-content: center; font-size: 18px;",
                        "↓"
                    }
                    h2 {
                        style: "margin: 0; font-size: 18px; font-weight: 600; color: #cdd6f4;",
                        "Save Download"
                    }
                }

                // Video title
                div {
                    style: "margin-bottom: 16px;",
                    label {
                        style: "display: block; font-size: 12px; color: #a6adc8; margin-bottom: 4px; text-transform: uppercase; letter-spacing: 0.05em;",
                        "File"
                    }
                    div {
                        style: "background: #1e1e2e; border: 1px solid #45475a; border-radius: 6px; padding: 8px 12px; font-size: 13px; color: #cdd6f4; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;",
                        "{video.text}"
                    }
                }

                // URL display
                div {
                    style: "margin-bottom: 20px;",
                    label {
                        style: "display: block; font-size: 12px; color: #a6adc8; margin-bottom: 4px; text-transform: uppercase; letter-spacing: 0.05em;",
                        "URL"
                    }
                    div {
                        style: "background: #1e1e2e; border: 1px solid #45475a; border-radius: 6px; padding: 8px 12px; font-size: 11px; color: #7f849c; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-family: monospace;",
                        "{video.url}"
                    }
                }

                // Save location picker
                div {
                    style: "margin-bottom: 20px;",
                    label {
                        style: "display: block; font-size: 12px; color: #a6adc8; margin-bottom: 4px; text-transform: uppercase; letter-spacing: 0.05em;",
                        "Save to"
                    }
                    div {
                        style: "display: flex; gap: 8px;",
                        input {
                            r#type: "text",
                            value: "{output_path}",
                            oninput: move |e| output_path.set(e.value()),
                            style: "flex: 1; background: #1e1e2e; border: 1px solid #45475a; border-radius: 6px; padding: 8px 12px; font-size: 13px; color: #cdd6f4; outline: none; font-family: monospace;",
                        }
                        button {
                            onclick: move |_| {
                                let current = output_path();
                                let current_path = std::path::PathBuf::from(&current);
                                let start_dir = current_path.parent()
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
                            style: "background: #45475a; color: #cdd6f4; border: none; border-radius: 6px; padding: 8px 14px; font-size: 13px; cursor: pointer; white-space: nowrap;",
                            "Browse..."
                        }
                    }
                }

                // Error message
                if !error_msg().is_empty() {
                    div {
                        style: "background: #45202a; border: 1px solid #f38ba8; border-radius: 6px; padding: 10px 14px; font-size: 13px; color: #f38ba8; margin-bottom: 16px;",
                        "{error_msg}"
                    }
                }

                // Action buttons
                div {
                    style: "display: flex; gap: 10px; justify-content: flex-end;",
                    button {
                        onclick: move |_| {
                            dioxus::desktop::window().close();
                        },
                        style: "background: transparent; color: #a6adc8; border: 1px solid #45475a; border-radius: 6px; padding: 9px 20px; font-size: 14px; cursor: pointer;",
                        "Cancel"
                    }
                    button {
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
                                            view.set(View::Progress {
                                                download_id: resp.id,
                                            });
                                        }
                                        Err(e) => {
                                            error_msg.set(format!("Failed to start download: {}", e));
                                            downloading.set(false);
                                        }
                                    }
                                });
                            }
                        },
                        style: "background: #89b4fa; color: #1e1e2e; border: none; border-radius: 6px; padding: 9px 24px; font-size: 14px; font-weight: 600; cursor: pointer;",
                        if downloading() { "Starting..." } else { "Download" }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// View 2 — Progress bar
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

    // Start SSE subscription once.
    let id_for_sse = download_id.clone();
    use_effect(move || {
        let id = id_for_sse.clone();
        spawn(async move {
            if let Err(e) = subscribe_progress(&id, move |snap| {
                snapshot.set(snap);
            })
            .await
            {
                error_msg.set(format!("Progress stream error: {}", e));
            }
        });
    });

    let snap = snapshot();
    let pct = if snap.total_bytes > 0 {
        (snap.total_bytes_downloaded as f64 / snap.total_bytes as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let speed_mb = snap.speed / (1024.0 * 1024.0);
    let downloaded_mb = snap.total_bytes_downloaded as f64 / (1024.0 * 1024.0);
    let total_mb = snap.total_bytes as f64 / (1024.0 * 1024.0);
    let is_done = snap.done;

    let eta_str = if snap.done {
        "Done".to_string()
    } else if snap.eta_secs > 0.0 {
        format_eta(snap.eta_secs)
    } else {
        "Calculating...".to_string()
    };

    let header_icon_style = if is_done {
        "width: 36px; height: 36px; background: #a6e3a1; border-radius: 8px; display: flex; align-items: center; justify-content: center; font-size: 18px;"
    } else {
        "width: 36px; height: 36px; background: #89b4fa; border-radius: 8px; display: flex; align-items: center; justify-content: center; font-size: 18px;"
    };

    let bar_color = if is_done { "#a6e3a1" } else { "#89b4fa" };
    let bar_style = format!(
        "background: {}; height: 100%; width: {}%; border-radius: 8px; transition: width 0.3s ease, background 0.3s;",
        bar_color, pct
    );

    rsx! {
        div {
            style: "font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #1e1e2e; color: #cdd6f4; min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: 24px;",
            div {
                style: "background: #313244; border-radius: 12px; padding: 28px 32px; width: 520px; box-shadow: 0 8px 32px rgba(0,0,0,0.4);",

                // Header
                div {
                    style: "display: flex; align-items: center; gap: 12px; margin-bottom: 20px;",
                    div {
                        style: "{header_icon_style}",
                        if is_done { "✓" } else { "↓" }
                    }
                    div {
                        h2 {
                            style: "margin: 0; font-size: 18px; font-weight: 600; color: #cdd6f4;",
                            if is_done { "Download Complete" } else { "Downloading..." }
                        }
                        p {
                            style: "margin: 2px 0 0; font-size: 13px; color: #a6adc8; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 380px;",
                            "{title}"
                        }
                    }
                }

                // Progress bar track
                div {
                    style: "background: #1e1e2e; border-radius: 8px; height: 10px; overflow: hidden; margin-bottom: 14px;",
                    div {
                        style: "{bar_style}",
                    }
                }

                // Stats row
                div {
                    style: "display: flex; justify-content: space-between; font-size: 13px; color: #a6adc8; margin-bottom: 20px;",
                    span { "{pct:.1}%" }
                    span {
                        if snap.total_bytes > 0 {
                            "{downloaded_mb:.1} / {total_mb:.1} MB"
                        } else {
                            "{downloaded_mb:.1} MB"
                        }
                    }
                    span {
                        if !is_done {
                            "{speed_mb:.2} MB/s"
                        }
                    }
                    span { "ETA: {eta_str}" }
                }

                // Error message
                if !error_msg().is_empty() {
                    div {
                        style: "background: #45202a; border: 1px solid #f38ba8; border-radius: 6px; padding: 10px 14px; font-size: 13px; color: #f38ba8; margin-bottom: 16px;",
                        "{error_msg}"
                    }
                }

                // Action buttons
                div {
                    style: "display: flex; justify-content: flex-end;",
                    if is_done {
                        button {
                            onclick: move |_| {
                                dioxus::desktop::window().close();
                            },
                            style: "background: #a6e3a1; color: #1e1e2e; border: none; border-radius: 6px; padding: 9px 24px; font-size: 14px; font-weight: 600; cursor: pointer;",
                            "Close"
                        }
                    } else {
                        button {
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
                            style: "background: #f38ba8; color: #1e1e2e; border: none; border-radius: 6px; padding: 9px 24px; font-size: 14px; font-weight: 600; cursor: pointer;",
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Derive a filename from the video title, falling back to the URL path.
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

    if base.ends_with(&format!(".{}", ext)) {
        base
    } else {
        format!("{}.{}", base, ext)
    }
}

fn ext_from_mime(mime: &str) -> Option<&'static str> {
    match mime {
        m if m.contains("mp4") => Some("mp4"),
        m if m.contains("webm") => Some("webm"),
        m if m.contains("mkv") => Some("mkv"),
        m if m.contains("avi") => Some("avi"),
        m if m.contains("mov") => Some("mov"),
        m if m.contains("mp3") => Some("mp3"),
        m if m.contains("ogg") => Some("ogg"),
        m if m.contains("flac") => Some("flac"),
        m if m.contains("wav") => Some("wav"),
        m if m.contains("m4v") => Some("m4v"),
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
