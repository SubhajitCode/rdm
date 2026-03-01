mod api;
mod app;

use api::VideoItem;
use app::App;
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;
use std::sync::OnceLock;

/// Set once before `launch()`, read by the root component.
static VIDEO_ITEM: OnceLock<VideoItem> = OnceLock::new();

fn main() {
    // Parse the --video-json argument passed by rdmd when it spawns us.
    let args: Vec<String> = std::env::args().collect();
    let video = parse_video_from_args(&args).unwrap_or_else(|e| {
        eprintln!("[rdm_ui] failed to parse video args: {}", e);
        std::process::exit(1);
    });

    let title = format!("RDM — {}", video.text);

    VIDEO_ITEM.set(video).expect("VIDEO_ITEM already set");

    dioxus::LaunchBuilder::new()
        .with_cfg(
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title(title)
                    .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(560.0_f64, 380.0_f64))
                    .with_resizable(false),
            ),
        )
        .launch(root);
}

fn root() -> Element {
    let video = VIDEO_ITEM.get().expect("VIDEO_ITEM not set").clone();
    rsx! {
        App { video }
    }
}

/// Parse the video item from CLI args.
/// Accepts:
///   rdm_ui --video-json '<json>'
fn parse_video_from_args(args: &[String]) -> Result<VideoItem, String> {
    let mut iter = args.iter().skip(1); // skip binary name
    while let Some(arg) = iter.next() {
        if arg == "--video-json" {
            let json = iter
                .next()
                .ok_or_else(|| "--video-json requires a value".to_string())?;
            return serde_json::from_str::<VideoItem>(json)
                .map_err(|e| format!("invalid JSON: {}", e));
        }
    }
    Err("Usage: rdm_ui --video-json '<json>'".to_string())
}
