mod api;
mod app;
mod styles;

use api::VideoItem;
use app::App;
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;
use std::sync::OnceLock;

/// Set once before `launch()`, read by the root component.
static VIDEO_ITEM: OnceLock<VideoItem> = OnceLock::new();

fn main() {
    // rdmd writes the VideoItem JSON to our stdin and closes the pipe.
    // We read it all before launching the Dioxus event loop.
    let video = read_video_from_stdin().unwrap_or_else(|e| {
        eprintln!("[rdm_ui] {}", e);
        std::process::exit(1);
    });

    let title = format!("RDM — {}", video.text);

    VIDEO_ITEM.set(video).expect("VIDEO_ITEM already set");

    LaunchBuilder::new()
        .with_cfg(
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title(title)
                    .with_inner_size(dioxus::desktop::tao::dpi::LogicalSize::new(480.0_f64, 310.0_f64))
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

/// Read the full stdin until EOF, then deserialize as a `VideoItem`.
///
/// rdmd writes the JSON and closes the pipe; we block here until EOF so we
/// have the complete payload before the Dioxus event loop starts.
fn read_video_from_stdin() -> Result<VideoItem, String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("failed to read stdin: {}", e))?;
    serde_json::from_str(buf.trim())
        .map_err(|e| format!("invalid JSON from stdin: {}\nraw: {}", e, buf))
}
