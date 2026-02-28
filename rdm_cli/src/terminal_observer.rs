use async_trait::async_trait;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::Mutex;

use rdm_core::progress::{format_bytes, ProgressObserver, ProgressSnapshot};

/// Renders download progress as indicatif terminal bars.
///
/// One `ProgressBar` is created per piece, plus a total bar.
/// All bars live under a shared `MultiProgress` so they render cleanly.
pub struct TerminalProgressObserver {
    multi: MultiProgress,
    /// piece_id → ProgressBar (lazily initialised on first `on_progress` call)
    bars: Mutex<HashMap<String, ProgressBar>>,
    /// The aggregate total bar
    total_bar: Mutex<Option<ProgressBar>>,
}

impl TerminalProgressObserver {
    pub fn new() -> Self {
        Self {
            multi: MultiProgress::new(),
            bars: Mutex::new(HashMap::new()),
            total_bar: Mutex::new(None),
        }
    }

    /// Ensure all per-piece bars and the total bar exist for the given snapshot.
    fn ensure_bars(&self, snapshot: &ProgressSnapshot) {
        let mut bars = self.bars.lock().unwrap();
        let mut total_bar = self.total_bar.lock().unwrap();

        // Per-piece bars
        for piece in &snapshot.pieces {
            if !bars.contains_key(&piece.piece_id) {
                let style = ProgressStyle::with_template(
                    "[{bar:30.cyan/blue}] {bytes}/{total_bytes} ({binary_bytes_per_sec}) ETA {eta} — {msg}",
                )
                .unwrap()
                .progress_chars("=>-");

                let pb = self.multi.add(ProgressBar::new(piece.total_bytes.max(1)));
                pb.set_style(style);
                pb.set_message(piece.piece_id.clone());
                bars.insert(piece.piece_id.clone(), pb);
            }
        }

        // Total bar (created once)
        if total_bar.is_none() && snapshot.total_bytes > 0 {
            let style = ProgressStyle::with_template(
                "Total [{bar:30.green/white}] {bytes}/{total_bytes} ({binary_bytes_per_sec}) ETA {eta}",
            )
            .unwrap()
            .progress_chars("=>-");

            let pb = self.multi.add(ProgressBar::new(snapshot.total_bytes.max(1)));
            pb.set_style(style);
            *total_bar = Some(pb);
        }
    }

    fn update_bars(&self, snapshot: &ProgressSnapshot) {
        let bars = self.bars.lock().unwrap();
        let total_bar = self.total_bar.lock().unwrap();

        for piece in &snapshot.pieces {
            if let Some(pb) = bars.get(&piece.piece_id) {
                pb.set_length(piece.total_bytes.max(1));
                pb.set_position(piece.bytes_downloaded);
            }
        }

        if let Some(pb) = total_bar.as_ref() {
            pb.set_length(snapshot.total_bytes.max(1));
            pb.set_position(snapshot.total_bytes_downloaded);
        }
    }

    fn finish_bars(&self, snapshot: &ProgressSnapshot) {
        let bars = self.bars.lock().unwrap();
        let total_bar = self.total_bar.lock().unwrap();

        for piece in &snapshot.pieces {
            if let Some(pb) = bars.get(&piece.piece_id) {
                pb.finish_with_message(format!("{} done", piece.piece_id));
            }
        }

        if let Some(pb) = total_bar.as_ref() {
            let speed = format_bytes(snapshot.speed as u64);
            let total = format_bytes(snapshot.total_bytes_downloaded);
            pb.finish_with_message(format!("Complete — {} at {}/s", total, speed));
        }
    }
}

#[async_trait]
impl ProgressObserver for TerminalProgressObserver {
    async fn on_progress(&self, snapshot: &ProgressSnapshot) {
        self.ensure_bars(snapshot);
        self.update_bars(snapshot);
    }

    async fn on_complete(&self, snapshot: &ProgressSnapshot) {
        self.ensure_bars(snapshot);
        self.finish_bars(snapshot);
    }

    async fn on_error(&self, error: &str) {
        // Abandon all open bars with the error message.
        let bars = self.bars.lock().unwrap();
        let total_bar = self.total_bar.lock().unwrap();

        for pb in bars.values() {
            pb.abandon_with_message(format!("Error: {}", error));
        }
        if let Some(pb) = total_bar.as_ref() {
            pb.abandon_with_message(format!("Failed: {}", error));
        }
    }
}
