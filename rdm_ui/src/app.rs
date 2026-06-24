use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use dioxus::prelude::*;

use crate::api::{
    delete_download_entry, delete_download_with_files, list_downloads, resume_download,
    stop_download, subscribe_progress, trigger_download, DownloadRequest, DownloadStatus,
    DownloadSummary, ProgressSnapshot, SegmentSnapshot, VideoItem,
};
use crate::styles::APP_CSS;

#[derive(Clone, Debug, PartialEq)]
pub enum AppMode {
    Dashboard,
    Download(VideoItem),
}

#[derive(Clone, Debug, PartialEq)]
enum View {
    FilePicker,
    Progress { download_id: String },
}

#[derive(Clone, Debug, PartialEq)]
struct ManualDownloadDraft {
    url: String,
    title: String,
    output_path: String,
}

#[component]
pub fn App(mode: AppMode) -> Element {
    let view = use_signal(|| View::FilePicker);

    rsx! {
        style { "{APP_CSS}" }
        match mode.clone() {
            AppMode::Dashboard => rsx! { DashboardView {} },
            AppMode::Download(video) => rsx! {
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
            },
        }
    }
}

#[component]
fn DashboardView() -> Element {
    let mut downloads = use_signal(Vec::<DownloadSummary>::new);
    let mut error_msg = use_signal(String::new);
    let busy_action = use_signal(|| None::<String>);
    let mut selected_ids = use_signal(HashSet::<String>::new);
    let mut show_manual = use_signal(|| false);
    let mut manual = use_signal(|| ManualDownloadDraft {
        url: String::new(),
        title: String::new(),
        output_path: default_manual_output_path(),
    });

    use_effect(move || {
        spawn(async move {
            loop {
                match list_downloads().await {
                    Ok(items) => {
                        let valid_ids: HashSet<String> =
                            items.iter().map(|download| download.id.clone()).collect();
                        let mut next_selected = selected_ids();
                        next_selected.retain(|id| valid_ids.contains(id));
                        selected_ids.set(next_selected);
                        downloads.set(items);
                        error_msg.set(String::new());
                    }
                    Err(err) => error_msg.set(format!("Failed to load downloads: {}", err)),
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    });

    let selected_count = selected_ids().len();
    let any_selected_active = downloads()
        .iter()
        .any(|download| selected_ids().contains(&download.id) && download.is_active);
    let all_selected = !downloads().is_empty() && selected_count == downloads().len();

    rsx! {
        div { class: "dashboard",
            div { class: "dashboard-header",
                div {
                    div { class: "dashboard-title", "Downloads" }
                    div { class: "dashboard-subtitle", "Resume, monitor, stop, or delete persisted downloads." }
                }
                div { class: "dashboard-toolbar",
                    button {
                        class: "btn btn--primary",
                        onclick: move |_| {
                            manual.set(ManualDownloadDraft {
                                url: String::new(),
                                title: String::new(),
                                output_path: default_manual_output_path(),
                            });
                            show_manual.set(true);
                        },
                        "New Download"
                    }
                    button {
                        class: "btn btn--ghost",
                        onclick: move |_| {
                            let mut downloads = downloads;
                            let mut error_msg = error_msg;
                            let mut selected_ids = selected_ids;
                            spawn(async move {
                                match list_downloads().await {
                                    Ok(items) => {
                                        let valid_ids: HashSet<String> = items.iter().map(|download| download.id.clone()).collect();
                                        let mut next_selected = selected_ids();
                                        next_selected.retain(|id| valid_ids.contains(id));
                                        selected_ids.set(next_selected);
                                        downloads.set(items);
                                        error_msg.set(String::new());
                                    }
                                    Err(err) => error_msg.set(format!("Failed to load downloads: {}", err)),
                                }
                            });
                        },
                        "Refresh"
                    }
                }
            }

            if !error_msg().is_empty() {
                div { class: "error-banner dashboard-error", "{error_msg}" }
            }

            if show_manual() {
                SaveDownloadWindow { manual, show_manual, error_msg }
            }

            if !downloads().is_empty() {
                div { class: "bulk-bar",
                    label { class: "bulk-checkbox",
                        input {
                            r#type: "checkbox",
                            checked: all_selected,
                            onchange: move |_| {
                                if all_selected {
                                    selected_ids.set(HashSet::new());
                                } else {
                                    selected_ids.set(downloads().iter().map(|download| download.id.clone()).collect());
                                }
                            }
                        }
                        span { "Select all" }
                    }
                    span { class: "bulk-count", "{selected_count} selected" }
                    div { class: "bulk-actions",
                        button {
                            class: "btn btn--cancel",
                            disabled: selected_count == 0 || any_selected_active,
                            onclick: move |_| {
                                let ids: Vec<String> = selected_ids().iter().cloned().collect();
                                let mut selected_ids = selected_ids;
                                let mut error_msg = error_msg;
                                spawn(async move {
                                    let mut failed = Vec::new();
                                    for id in ids {
                                        if delete_download_entry(&id).await.is_err() {
                                            failed.push(id);
                                        }
                                    }
                                    selected_ids.set(HashSet::new());
                                    if failed.is_empty() {
                                        error_msg.set(String::new());
                                    } else {
                                        error_msg.set(format!("Failed to delete {} selected entr{}.", failed.len(), if failed.len() == 1 { "y" } else { "ies" }));
                                    }
                                });
                            },
                            "Delete Selected"
                        }
                        button {
                            class: "btn btn--danger",
                            disabled: selected_count == 0 || any_selected_active,
                            onclick: move |_| {
                                let ids: Vec<String> = selected_ids().iter().cloned().collect();
                                let mut selected_ids = selected_ids;
                                let mut error_msg = error_msg;
                                spawn(async move {
                                    let mut failed = Vec::new();
                                    for id in ids {
                                        if delete_download_with_files(&id).await.is_err() {
                                            failed.push(id);
                                        }
                                    }
                                    selected_ids.set(HashSet::new());
                                    if failed.is_empty() {
                                        error_msg.set(String::new());
                                    } else {
                                        error_msg.set(format!("Failed to delete files for {} selected download{}.", failed.len(), if failed.len() == 1 { "" } else { "s" }));
                                    }
                                });
                            },
                            "Delete Selected Files"
                        }
                    }
                }
            }

            div { class: "download-list",
                if downloads().is_empty() {
                    div { class: "empty-state",
                        div { class: "empty-state__title", "No downloads yet" }
                        div { class: "empty-state__subtitle", "Use New Download or start a browser-triggered download and it will appear here." }
                    }
                } else {
                    for download in downloads().iter() {
                        { dashboard_card(download.clone(), busy_action, error_msg, selected_ids) }
                    }
                }
            }
        }
    }
}

fn dashboard_card(
    download: DownloadSummary,
    mut busy_action: Signal<Option<String>>,
    mut error_msg: Signal<String>,
    mut selected_ids: Signal<HashSet<String>>,
) -> Element {
    let pct = if download.total_bytes > 0 {
        (download.total_bytes_downloaded as f64 / download.total_bytes as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let busy = busy_action()
        .as_ref()
        .map(|value| value.starts_with(&download.id))
        .unwrap_or(false);
    let checked = selected_ids().contains(&download.id);
    let download_id = download.id.clone();

    rsx! {
        div { class: "download-card",
            div { class: "download-card__top",
                div { class: "download-card__head",
                    label { class: "card-checkbox",
                        input {
                            r#type: "checkbox",
                            checked: checked,
                            onchange: move |_| {
                                let mut next = selected_ids();
                                if next.contains(&download_id) {
                                    next.remove(&download_id);
                                } else {
                                    next.insert(download_id.clone());
                                }
                                selected_ids.set(next);
                            }
                        }
                    }
                    div { class: "download-card__meta",
                        div { class: "download-card__title", "{download.title}" }
                        div { class: "download-card__path", "{download.output_path}" }
                    }
                }
                span { class: "status-pill {status_class(&download.status)}", "{status_label(&download.status)}" }
            }

            div { class: "download-card__url", "{download.url}" }

            div { class: "bar-track download-card__bar",
                div {
                    class: if matches!(download.status, DownloadStatus::Completed) { "bar-fill bar-fill--green" } else { "bar-fill bar-fill--blue" },
                    style: "width: {pct:.2}%;",
                }
            }

            div { class: "download-card__stats",
                div { class: "download-card__stat", "{format_progress(&download)}" }
                div { class: "download-card__stat", "{format_speed(download.speed)}" }
                div { class: "download-card__stat", "{format_eta_or_status(&download)}" }
            }

            if let Some(err) = &download.last_error {
                if !err.is_empty() {
                    div { class: "download-card__error", "{err}" }
                }
            }

            div { class: "download-card__actions",
                if download.file_exists {
                    button {
                        class: "btn btn--ghost",
                        disabled: busy,
                        onclick: {
                            let output_path = download.output_path.clone();
                            move |_| {
                                match open_download_file(&output_path) {
                                    Ok(()) => error_msg.set(String::new()),
                                    Err(err) => error_msg.set(format!("Failed to open file: {}", err)),
                                }
                            }
                        },
                        "Open File"
                    }
                }

                if matches!(download.status, DownloadStatus::Running) {
                    button {
                        class: "btn btn--danger-lite",
                        disabled: busy,
                        onclick: {
                            let id = download.id.clone();
                            move |_| {
                                busy_action.set(Some(format!("{}:stop", id)));
                                let request_id = id.clone();
                                spawn(async move {
                                    let _ = stop_download(&request_id).await;
                                    busy_action.set(None);
                                });
                            }
                        },
                        "Stop"
                    }
                } else if download.can_resume {
                    button {
                        class: "btn btn--primary",
                        disabled: busy,
                        onclick: {
                            let id = download.id.clone();
                            move |_| {
                                busy_action.set(Some(format!("{}:resume", id)));
                                let request_id = id.clone();
                                spawn(async move {
                                    let _ = resume_download(&request_id).await;
                                    busy_action.set(None);
                                });
                            }
                        },
                        "Resume"
                    }
                }

                button {
                    class: "btn btn--cancel",
                    disabled: busy || download.is_active,
                    onclick: {
                        let id = download.id.clone();
                        move |_| {
                            busy_action.set(Some(format!("{}:delete-entry", id)));
                            let request_id = id.clone();
                            spawn(async move {
                                let _ = delete_download_entry(&request_id).await;
                                busy_action.set(None);
                            });
                        }
                    },
                    "Delete Entry"
                }

                button {
                    class: "btn btn--danger",
                    disabled: busy || download.is_active,
                    onclick: {
                        let id = download.id.clone();
                        move |_| {
                            busy_action.set(Some(format!("{}:delete-files", id)));
                            let request_id = id.clone();
                            spawn(async move {
                                let _ = delete_download_with_files(&request_id).await;
                                busy_action.set(None);
                            });
                        }
                    },
                    "Delete Files"
                }
            }
        }
    }
}

#[component]
fn FilePickerView(video: VideoItem, mut view: Signal<View>) -> Element {
    let default_filename = derive_filename(&video.text, &video.url, video.info.as_str());
    let default_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("rdm");
    let default_path = default_dir.join(&default_filename);

    let mut output_path = use_signal(|| default_path.to_string_lossy().to_string());
    let mut error_msg = use_signal(String::new);
    let mut downloading = use_signal(|| false);
    let video_clone = video.clone();

    rsx! {
        div { class: "view",
            div { class: "header",
                div { class: "header-icon header-icon--blue", "↓" }
                div { class: "header-text",
                    div { class: "header-title", "Save Download" }
                    div { class: "header-subtitle", "{video.text}" }
                }
            }

            div { class: "divider divider--top" }

            div { class: "field",
                div { class: "field-label", "Source URL" }
                div { class: "field-value", "{video.url}" }
            }

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
                        "Browse…"
                    }
                }
            }

            if !error_msg().is_empty() {
                div { class: "error-banner", "{error_msg}" }
            }

            div { class: "spacer" }
            div { class: "divider divider--bottom" }

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
                                id: video_for_download.id.clone(),
                                url: video_for_download.url.clone(),
                                title: video_for_download.text.clone(),
                                output_path: path.clone(),
                                cookie: video_for_download.cookie.clone(),
                                request_headers: video_for_download.request_headers.clone(),
                                user_agent: video_for_download.user_agent.clone(),
                                referer: video_for_download.referer.clone(),
                                info: video_for_download.info.clone(),
                            };

                            spawn(async move {
                                match trigger_download(&req).await {
                                    Ok(resp) => view.set(View::Progress { download_id: resp.id }),
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

#[component]
fn ProgressView(download_id: String, title: String) -> Element {
    let mut snapshot = use_signal(|| ProgressSnapshot {
        segments: Vec::new(),
        total_bytes_downloaded: 0,
        total_bytes: 0,
        speed: 0.0,
        eta_secs: 0.0,
        done: false,
    });
    let mut error_msg = use_signal(String::new);
    let mut show_segments = use_signal(|| false);

    let id_for_sse = download_id.clone();
    use_effect(move || {
        let id = id_for_sse.clone();
        spawn(async move {
            if let Err(e) = subscribe_progress(&id, move |snap| snapshot.set(snap)).await {
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
    let downloaded_mb = snap.total_bytes_downloaded as f64 / (1024.0 * 1024.0);
    let total_mb = snap.total_bytes as f64 / (1024.0 * 1024.0);
    let is_done = snap.done;
    let eta_str = if is_done {
        "Complete".to_string()
    } else if snap.eta_secs > 0.0 {
        format_eta(snap.eta_secs)
    } else {
        "Calculating…".to_string()
    };

    rsx! {
        div { class: "view",
            div { class: "header",
                div {
                    class: if is_done { "header-icon header-icon--green" } else { "header-icon header-icon--blue" },
                    if is_done { "✓" } else { "↓" }
                }
                div { class: "header-text",
                    div { class: "header-title", if is_done { "Download Complete" } else { "Downloading…" } }
                    div { class: "header-subtitle", "{title}" }
                }
            }

            div { class: "divider divider--top" }

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
                        style: "width: {pct:.2}%;",
                    }
                }

                if snap.segments.len() > 1 {
                    button {
                        class: "segments-toggle",
                        onclick: move |_| show_segments.set(!show_segments()),
                        if show_segments() { "▾ Segments" } else { "▸ Segments" }
                    }
                }

                if snap.segments.len() > 1 && show_segments() {
                    div { class: "segments-panel",
                        for seg in snap.segments.iter() {
                            { segment_bar(seg) }
                        }
                    }
                }
            }

            div { class: "stats-row",
                div { class: "stat-card",
                    div { class: "stat-label", "Speed" }
                    div { class: "stat-value", if is_done { "—" } else { "{format_speed(snap.speed)}" } }
                }
                div { class: "stat-card",
                    div { class: "stat-label", "ETA" }
                    div { class: "stat-value", "{eta_str}" }
                }
            }

            if !error_msg().is_empty() {
                div { class: "error-banner", style: "margin-top: 14px;", "{error_msg}" }
            }

            div { class: "spacer" }
            div { class: "divider divider--bottom" }

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
                                    let _ = stop_download(&id).await;
                                    dioxus::desktop::window().close();
                                });
                            }
                        },
                        "Stop"
                    }
                }
            }
        }
    }
}

fn derive_filename(title: &str, url: &str, mime: &str) -> String {
    let base = if !title.is_empty() {
        title
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
            .collect::<String>()
            .trim()
            .to_string()
    } else {
        url.rsplit('/').find(|s| !s.is_empty()).unwrap_or("download").to_string()
    };

    let ext = ext_from_mime(mime)
        .or_else(|| ext_from_url(url))
        .unwrap_or("mp4");

    let base = if base.chars().count() > 50 {
        base.chars().take(50).collect::<String>().trim_end().to_string()
    } else {
        base
    };

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
            "mp4" => Some("mp4"),
            "webm" => Some("webm"),
            "mkv" => Some("mkv"),
            "avi" => Some("avi"),
            "mov" => Some("mov"),
            "mp3" => Some("mp3"),
            "ogg" => Some("ogg"),
            "flac" => Some("flac"),
            "wav" => Some("wav"),
            "m4v" => Some("m4v"),
            _ => None,
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

fn format_speed(bps: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bps >= GB {
        format!("{:.2} GB/s", bps / GB)
    } else if bps >= MB {
        format!("{:.2} MB/s", bps / MB)
    } else if bps >= KB {
        format!("{:.1} KB/s", bps / KB)
    } else {
        format!("{:.0} B/s", bps)
    }
}

fn segment_bar(seg: &SegmentSnapshot) -> Element {
    const MB: f64 = 1024.0 * 1024.0;
    let start_mb = seg.offset as f64 / MB;
    let end_mb = (seg.offset + seg.total_bytes) as f64 / MB;
    let label = if seg.total_bytes > 0 {
        format!("{:.0}–{:.0} MB", start_mb, end_mb)
    } else {
        "…".to_string()
    };

    let pct = if seg.total_bytes > 0 {
        (seg.bytes_downloaded as f64 / seg.total_bytes as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let is_done = seg.bytes_downloaded >= seg.total_bytes && seg.total_bytes > 0;

    rsx! {
        div { class: "seg-row",
            span { class: "seg-label", "{label}" }
            div { class: "seg-bar-track",
                div {
                    class: if is_done { "seg-bar-fill seg-bar-fill--green" } else { "seg-bar-fill" },
                    style: "width: {pct:.2}%;",
                }
            }
            span { class: "seg-speed", if is_done { "—" } else { "{format_speed(seg.speed)}" } }
        }
    }
}

fn format_progress(download: &DownloadSummary) -> String {
    if download.total_bytes > 0 {
        format!(
            "{} / {}",
            format_bytes(download.total_bytes_downloaded),
            format_bytes(download.total_bytes)
        )
    } else {
        format!("{} downloaded", format_bytes(download.total_bytes_downloaded))
    }
}

fn format_eta_or_status(download: &DownloadSummary) -> String {
    match download.status {
        DownloadStatus::Completed => "Complete".to_string(),
        DownloadStatus::Running if download.eta_secs > 0.0 => format_eta(download.eta_secs),
        DownloadStatus::Running => "Calculating…".to_string(),
        DownloadStatus::Stopped => "Stopped".to_string(),
        DownloadStatus::Interrupted => "Interrupted".to_string(),
        DownloadStatus::Failed => "Failed".to_string(),
        DownloadStatus::Queued => "Queued".to_string(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn status_label(status: &DownloadStatus) -> &'static str {
    match status {
        DownloadStatus::Queued => "Queued",
        DownloadStatus::Running => "Running",
        DownloadStatus::Stopped => "Stopped",
        DownloadStatus::Completed => "Completed",
        DownloadStatus::Failed => "Failed",
        DownloadStatus::Interrupted => "Interrupted",
    }
}

fn status_class(status: &DownloadStatus) -> &'static str {
    match status {
        DownloadStatus::Queued => "status-pill--queued",
        DownloadStatus::Running => "status-pill--running",
        DownloadStatus::Stopped => "status-pill--stopped",
        DownloadStatus::Completed => "status-pill--completed",
        DownloadStatus::Failed => "status-pill--failed",
        DownloadStatus::Interrupted => "status-pill--interrupted",
    }
}

fn open_download_file(path: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open")
        .arg(path)
        .status()
        .map_err(|e| format!("failed to launch open: {}", e))?;

    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", path])
        .status()
        .map_err(|e| format!("failed to launch start: {}", e))?;

    #[cfg(all(unix, not(target_os = "macos")))]
    let status = std::process::Command::new("xdg-open")
        .arg(path)
        .status()
        .map_err(|e| format!("failed to launch xdg-open: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with status {}", status))
    }
}

#[component]
fn SaveDownloadWindow(
    mut manual: Signal<ManualDownloadDraft>,
    mut show_manual: Signal<bool>,
    mut error_msg: Signal<String>,
) -> Element {
    let mut starting = use_signal(|| false);

    rsx! {
        div { class: "modal-backdrop",
            div { class: "modal-card",
                div { class: "header",
                    div { class: "header-icon header-icon--blue", "↓" }
                    div { class: "header-text",
                        div { class: "header-title", "Save Download" }
                        div { class: "header-subtitle", "Create a new download from the dashboard." }
                    }
                }

                div { class: "divider divider--top" }

                div { class: "field",
                    div { class: "field-label", "Source URL" }
                    input {
                        r#type: "text",
                        class: "path-input path-input--wide",
                        value: "{manual().url}",
                        oninput: move |e| {
                            let mut draft = manual();
                            draft.url = e.value();
                            if draft.title.trim().is_empty() {
                                draft.title = derive_manual_title(&draft.url);
                            }
                            if draft.output_path.trim().is_empty() || draft.output_path == default_manual_output_path() {
                                draft.output_path = default_manual_output_path_for(&draft.title, &draft.url);
                            }
                            manual.set(draft);
                        }
                    }
                }

                div { class: "field",
                    div { class: "field-label", "File name" }
                    input {
                        r#type: "text",
                        class: "path-input path-input--wide",
                        value: "{manual().title}",
                        oninput: move |e| {
                            let mut draft = manual();
                            draft.title = e.value();
                            manual.set(draft);
                        }
                    }
                }

                div { class: "field",
                    div { class: "field-label", "Save to" }
                    div { class: "path-row",
                        input {
                            r#type: "text",
                            class: "path-input",
                            value: "{manual().output_path}",
                            oninput: move |e| {
                                let mut draft = manual();
                                draft.output_path = e.value();
                                manual.set(draft);
                            }
                        }
                        button {
                            class: "btn btn--browse",
                            onclick: move |_| {
                                let current = manual().output_path;
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
                                    let mut draft = manual();
                                    draft.output_path = path.to_string_lossy().to_string();
                                    manual.set(draft);
                                }
                            },
                            "Browse…"
                        }
                    }
                }

                div { class: "divider divider--bottom" }

                div { class: "btn-row",
                    button {
                        class: "btn btn--cancel",
                        disabled: starting(),
                        onclick: move |_| show_manual.set(false),
                        "Cancel"
                    }
                    button {
                        class: "btn btn--primary",
                        disabled: starting(),
                        onclick: move |_| {
                            let draft = manual();
                            if draft.url.trim().is_empty() {
                                error_msg.set("Please enter a download URL.".to_string());
                                return;
                            }
                            if draft.output_path.trim().is_empty() {
                                error_msg.set("Please choose where to save the file.".to_string());
                                return;
                            }

                            starting.set(true);
                            let title = if draft.title.trim().is_empty() {
                                derive_manual_title(&draft.url)
                            } else {
                                draft.title.trim().to_string()
                            };
                            let req = DownloadRequest {
                                id: generate_manual_id(),
                                url: draft.url.trim().to_string(),
                                title: title.clone(),
                                output_path: draft.output_path.trim().to_string(),
                                cookie: String::new(),
                                request_headers: Default::default(),
                                user_agent: None,
                                referer: None,
                                info: String::new(),
                            };

                            spawn(async move {
                                match trigger_download(&req).await {
                                    Ok(_) => {
                                        error_msg.set(String::new());
                                        show_manual.set(false);
                                        manual.set(ManualDownloadDraft {
                                            url: String::new(),
                                            title: String::new(),
                                            output_path: default_manual_output_path(),
                                        });
                                    }
                                    Err(err) => error_msg.set(format!("Failed to start download: {}", err)),
                                }
                                starting.set(false);
                            });
                        },
                        if starting() { "Starting…" } else { "Download" }
                    }
                }
            }
        }
    }
}

fn default_manual_output_path() -> String {
    default_manual_output_path_for("download", "")
}

fn default_manual_output_path_for(title: &str, url: &str) -> String {
    let default_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("rdm");
    default_dir
        .join(derive_filename(title, url, ""))
        .to_string_lossy()
        .to_string()
}

fn derive_manual_title(url: &str) -> String {
    url.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("download")
        .split('?')
        .next()
        .unwrap_or("download")
        .to_string()
}

fn generate_manual_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("manual-{}", millis)
}
