use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::types::types::ProgressEvent;

// ---------------------------------------------------------------------------
// Snapshot types — serializable, shared with HTTP consumers
// ---------------------------------------------------------------------------

/// Per-piece progress snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct PieceSnapshot {
    pub piece_id: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
}

/// Aggregate progress snapshot for an entire download.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressSnapshot {
    pub pieces: Vec<PieceSnapshot>,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_secs: f64,
    pub done: bool,
}

impl ProgressSnapshot {
    pub fn empty() -> Self {
        Self {
            pieces: Vec::new(),
            total_bytes_downloaded: 0,
            total_bytes: 0,
            speed: 0.0,
            eta_secs: 0.0,
            done: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal per-piece tracking
// ---------------------------------------------------------------------------

struct PieceProgress {
    piece_id: String,
    bytes_downloaded: u64,
    total_bytes: u64,
    speed: f64,
    last_update: Instant,
    bar: ProgressBar,
}

// ---------------------------------------------------------------------------
// ProgressAggregator
// ---------------------------------------------------------------------------

/// Consumes `ProgressEvent`s from the download channel, drives `indicatif`
/// progress bars in the terminal, and maintains a shared `ProgressSnapshot`
/// that can be polled by HTTP endpoints.
pub struct ProgressAggregator {
    multi: MultiProgress,
    pieces: HashMap<String, PieceProgress>,
    total_bar: ProgressBar,
    snapshot: Arc<RwLock<ProgressSnapshot>>,
    start_time: Instant,
    /// Insertion-order tracking so snapshot pieces are stable.
    piece_order: Vec<String>,
}

/// EMA smoothing factor. 0.3 = responsive but stable.
const EMA_ALPHA: f64 = 0.3;

impl ProgressAggregator {
    /// Create a new aggregator.
    ///
    /// Returns `(aggregator, snapshot_handle)` — the snapshot handle can be
    /// stored in `ActiveDownload` and polled by SSE endpoints.
    pub fn new() -> (Self, Arc<RwLock<ProgressSnapshot>>) {
        let multi = MultiProgress::new();
        let snapshot = Arc::new(RwLock::new(ProgressSnapshot::empty()));

        // Total bar — added last (will be repositioned below piece bars).
        let total_style = ProgressStyle::with_template(
            "{prefix} [{wide_bar:.cyan/blue}] {percent:>3}% {binary_bytes_per_sec} ETA {eta}",
        )
        .unwrap()
        .progress_chars("█░");

        let total_bar = multi.add(ProgressBar::new(0));
        total_bar.set_style(total_style);
        total_bar.set_prefix("[total]  ");

        let agg = Self {
            multi,
            pieces: HashMap::new(),
            total_bar,
            snapshot: Arc::clone(&snapshot),
            start_time: Instant::now(),
            piece_order: Vec::new(),
        };

        (agg, snapshot)
    }

    /// Consume progress events until the channel closes, then print a summary.
    pub async fn run(mut self, mut progress_rx: mpsc::Receiver<ProgressEvent>) {
        while let Some(ev) = progress_rx.recv().await {
            self.handle_event(ev);
        }
        self.finish();
    }

    /// Process a single progress event.
    fn handle_event(&mut self, ev: ProgressEvent) {
        let now = Instant::now();

        // Lazy init: create a bar the first time we see a piece_id.
        if !self.pieces.contains_key(&ev.piece_id) {
            let total = ev.total_bytes.unwrap_or(0);
            let idx = self.piece_order.len();

            let piece_style = ProgressStyle::with_template(
                "{prefix} [{wide_bar:.green/dark.green}] {percent:>3}% {binary_bytes_per_sec} ETA {eta}",
            )
            .unwrap()
            .progress_chars("█░");

            // Insert before the total bar.
            let bar = self.multi.insert_before(&self.total_bar, ProgressBar::new(total));
            bar.set_style(piece_style);
            bar.set_prefix(format!("[piece {}]", idx + 1));

            self.piece_order.push(ev.piece_id.clone());
            self.pieces.insert(
                ev.piece_id.clone(),
                PieceProgress {
                    piece_id: ev.piece_id.clone(),
                    bytes_downloaded: 0,
                    total_bytes: total,
                    speed: 0.0,
                    last_update: now,
                    bar,
                },
            );

            // Update total bar length (sum of all piece totals).
            let total_len: u64 = self.pieces.values().map(|p| p.total_bytes).sum();
            self.total_bar.set_length(total_len);
        }

        // Update the piece state — scoped block to drop the mutable borrow.
        let mut recalc_total_len = false;
        {
            let piece = self.pieces.get_mut(&ev.piece_id).unwrap();

            // Accumulate bytes.
            piece.bytes_downloaded += ev.bytes_delta;

            // Update total_bytes if we didn't know it before.
            if piece.total_bytes == 0 {
                if let Some(tb) = ev.total_bytes {
                    piece.total_bytes = tb;
                    piece.bar.set_length(tb);
                    recalc_total_len = true;
                }
            }

            // Compute EMA speed.
            let elapsed = now.duration_since(piece.last_update).as_secs_f64();
            if elapsed > 0.0 {
                let instant_speed = ev.bytes_delta as f64 / elapsed;
                piece.speed = EMA_ALPHA * instant_speed + (1.0 - EMA_ALPHA) * piece.speed;
                piece.last_update = now;
            }

            // Update indicatif bar.
            piece.bar.set_position(piece.bytes_downloaded);
        }

        // Recalculate total bar length if a piece's total_bytes was just discovered.
        if recalc_total_len {
            let total_len: u64 = self.pieces.values().map(|p| p.total_bytes).sum();
            self.total_bar.set_length(total_len);
        }

        // Update total bar.
        let total_downloaded: u64 = self.pieces.values().map(|p| p.bytes_downloaded).sum();
        self.total_bar.set_position(total_downloaded);

        // Update shared snapshot.
        self.update_snapshot(total_downloaded);
    }

    /// Rebuild the shared snapshot from current state.
    fn update_snapshot(&self, total_downloaded: u64) {
        let total_bytes: u64 = self.pieces.values().map(|p| p.total_bytes).sum();
        let combined_speed: f64 = self.pieces.values().map(|p| p.speed).sum();
        let remaining = if total_bytes > total_downloaded {
            total_bytes - total_downloaded
        } else {
            0
        };
        let eta = if combined_speed > 0.0 {
            remaining as f64 / combined_speed
        } else {
            0.0
        };

        let piece_snapshots: Vec<PieceSnapshot> = self
            .piece_order
            .iter()
            .filter_map(|id| self.pieces.get(id))
            .map(|p| {
                let remaining = if p.total_bytes > p.bytes_downloaded {
                    p.total_bytes - p.bytes_downloaded
                } else {
                    0
                };
                let eta = if p.speed > 0.0 {
                    remaining as f64 / p.speed
                } else {
                    0.0
                };
                PieceSnapshot {
                    piece_id: p.piece_id.clone(),
                    bytes_downloaded: p.bytes_downloaded,
                    total_bytes: p.total_bytes,
                    speed: p.speed,
                    eta_secs: eta,
                }
            })
            .collect();

        let snap = ProgressSnapshot {
            pieces: piece_snapshots,
            total_bytes_downloaded: total_downloaded,
            total_bytes,
            speed: combined_speed,
            eta_secs: eta,
            done: false,
        };

        if let Ok(mut guard) = self.snapshot.write() {
            *guard = snap;
        }
    }

    /// Finalize: finish all bars, print summary, mark snapshot as done.
    fn finish(self) {
        let elapsed = self.start_time.elapsed();
        let total_downloaded: u64 = self.pieces.values().map(|p| p.bytes_downloaded).sum();

        // Finish piece bars.
        for id in &self.piece_order {
            if let Some(p) = self.pieces.get(id) {
                p.bar.finish();
            }
        }
        self.total_bar.finish();

        // Print summary below the bars.
        let avg_speed = if elapsed.as_secs_f64() > 0.0 {
            total_downloaded as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        self.multi.println(format!(
            "\nDownload complete: {} in {:.1}s (avg {}/s)",
            format_bytes(total_downloaded),
            elapsed.as_secs_f64(),
            format_bytes(avg_speed as u64),
        )).ok();

        // Mark snapshot as done.
        if let Ok(mut guard) = self.snapshot.write() {
            guard.done = true;
            guard.total_bytes_downloaded = total_downloaded;
            guard.speed = avg_speed;
            guard.eta_secs = 0.0;
        }
    }
}

/// Human-readable byte formatting.
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
