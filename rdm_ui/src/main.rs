mod api;
mod app;
mod server_bootstrap;
mod styles;

use std::io::{IsTerminal, Read};
use std::sync::OnceLock;

use app::{App, AppMode};
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;

static APP_MODE: OnceLock<AppMode> = OnceLock::new();
const DOWNLOAD_STDIN_FLAG: &str = "--download-stdin";
const DOWNLOAD_STDIN_ENV: &str = "RDM_UI_MODE";
const DOWNLOAD_STDIN_ENV_VALUE: &str = "download-stdin";

fn main() {
    let mode = read_launch_mode().unwrap_or_else(|e| {
        eprintln!("[rdm_ui] {}", e);
        std::process::exit(1);
    });

    if matches!(mode, AppMode::Dashboard) {
        server_bootstrap::ensure_server_running().unwrap_or_else(|e| {
            eprintln!("[rdm_ui] {}", e);
            std::process::exit(1);
        });
    }

    let (title, width, height, resizable) = match &mode {
        AppMode::Dashboard => ("RDM Dashboard".to_string(), 980.0, 720.0, true),
        AppMode::Download(video) => (format!("RDM — {}", video.text), 480.0, 310.0, false),
    };

    APP_MODE.set(mode).expect("APP_MODE already set");

    LaunchBuilder::new()
        .with_cfg(
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title(title)
                    .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(width, height))
                    .with_resizable(resizable),
            ),
        )
        .launch(root);
}

fn root() -> Element {
    let mode = APP_MODE.get().expect("APP_MODE not set").clone();
    rsx! {
        App { mode }
    }
}

fn read_launch_mode() -> Result<AppMode, String> {
    let expects_download_payload = std::env::args().any(|arg| arg == DOWNLOAD_STDIN_FLAG);
    let expects_download_env = std::env::var(DOWNLOAD_STDIN_ENV)
        .map(|value| value == DOWNLOAD_STDIN_ENV_VALUE)
        .unwrap_or(false);
    let explicit_download_mode = expects_download_payload || expects_download_env;

    if std::io::stdin().is_terminal() {
        if explicit_download_mode {
            return Err("download stdin mode requested but no stdin payload was provided".to_string());
        }
        return Ok(AppMode::Dashboard);
    }

    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("failed to read stdin: {}", e))?;

    if buf.trim().is_empty() {
        if explicit_download_mode {
            return Err("download stdin mode requested but stdin payload was empty".to_string());
        }
        return Ok(AppMode::Dashboard);
    }

    let video = serde_json::from_str(buf.trim());
    if explicit_download_mode || launched_by_rdmd() {
        let video = video.map_err(|e| format!("invalid JSON from stdin: {}\nraw: {}", e, buf))?;
        return Ok(AppMode::Download(video));
    }

    Ok(AppMode::Dashboard)
}

fn launched_by_rdmd() -> bool {
    #[cfg(unix)]
    {
        let current_pid = std::process::id().to_string();
        let parent_pid = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &current_pid])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_string());

        let Some(parent_pid) = parent_pid else {
            return false;
        };

        return std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &parent_pid])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().ends_with("rdmd"))
            .unwrap_or(false);
    }

    #[cfg(not(unix))]
    {
        false
    }
}
