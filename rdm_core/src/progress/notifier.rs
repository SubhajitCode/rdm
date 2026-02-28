use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::types::types::ProgressEvent;
use super::observer::ProgressObserver;
use super::snapshot::{PieceSnapshot, ProgressSnapshot};

/// EMA smoothing factor. 0.3 = responsive but stable.
const EMA_ALPHA: f64 = 0.3;

/// Internal per-piece tracking (purely data, no UI).
struct PieceProgress {
    piece_id: String,
    bytes_downloaded: u64,
    total_bytes: u64,
    speed: f64,
    last_update: Instant,
}

/// Consumes `Result<ProgressEvent, String>` from the download channel,
/// aggregates progress into `ProgressSnapshot`s, and fans out to all
/// registered observers.
///
/// # Lifecycle
///
/// | Channel message        | Observer method called          |
/// |------------------------|---------------------------------|
/// | `Ok(ProgressEvent)`    | `on_progress(&snapshot)`        |
/// | `Err(String)`          | `on_error(&msg)` then stops     |
/// | Channel closed (no err)| `on_complete(&final_snapshot)`  |
pub struct ProgressNotifier {
    observers: Vec<Box<dyn ProgressObserver>>,
    pieces: HashMap<String, PieceProgress>,
    piece_order: Vec<String>,
    start_time: Instant,
}

impl ProgressNotifier {
    pub fn new() -> Self {
        Self {
            observers: Vec::new(),
            pieces: HashMap::new(),
            piece_order: Vec::new(),
            start_time: Instant::now(),
        }
    }

    /// Register an observer. Must be called before `run()`.
    pub fn add_observer(&mut self, observer: Box<dyn ProgressObserver>) {
        self.observers.push(observer);
    }

    /// Consume progress messages until the channel closes or an error arrives.
    pub async fn run(
        mut self,
        mut progress_rx: mpsc::Receiver<Result<ProgressEvent, String>>,
    ) {
        while let Some(msg) = progress_rx.recv().await {
            match msg {
                Ok(ev) => {
                    let snapshot = self.handle_event(ev);
                    for observer in &self.observers {
                        observer.on_progress(&snapshot).await;
                    }
                }
                Err(error) => {
                    for observer in &self.observers {
                        observer.on_error(&error).await;
                    }
                    return; // stop processing after error
                }
            }
        }
        // Channel closed cleanly — all senders dropped, no error received
        self.finish().await;
    }

    /// Process a single progress event and return the updated snapshot.
    fn handle_event(&mut self, ev: ProgressEvent) -> ProgressSnapshot {
        let now = Instant::now();

        // Lazy init: track new piece_id on first sight
        if !self.pieces.contains_key(&ev.piece_id) {
            let total = ev.total_bytes.unwrap_or(0);
            self.piece_order.push(ev.piece_id.clone());
            self.pieces.insert(
                ev.piece_id.clone(),
                PieceProgress {
                    piece_id: ev.piece_id.clone(),
                    bytes_downloaded: 0,
                    total_bytes: total,
                    speed: 0.0,
                    last_update: now,
                },
            );
        }

        // Update the piece state
        {
            let piece = self.pieces.get_mut(&ev.piece_id).unwrap();
            piece.bytes_downloaded += ev.bytes_delta;

            // Update total_bytes if we didn't know it before
            if piece.total_bytes == 0 {
                if let Some(tb) = ev.total_bytes {
                    piece.total_bytes = tb;
                }
            }

            // Compute EMA speed
            let elapsed = now.duration_since(piece.last_update).as_secs_f64();
            if elapsed > 0.0 {
                let instant_speed = ev.bytes_delta as f64 / elapsed;
                piece.speed = EMA_ALPHA * instant_speed + (1.0 - EMA_ALPHA) * piece.speed;
                piece.last_update = now;
            }
        }

        self.build_snapshot()
    }

    /// Build a `ProgressSnapshot` from current aggregation state.
    fn build_snapshot(&self) -> ProgressSnapshot {
        let total_bytes: u64 = self.pieces.values().map(|p| p.total_bytes).sum();
        let total_downloaded: u64 = self.pieces.values().map(|p| p.bytes_downloaded).sum();
        let combined_speed: f64 = self.pieces.values().map(|p| p.speed).sum();
        let remaining = total_bytes.saturating_sub(total_downloaded);
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
                let rem = p.total_bytes.saturating_sub(p.bytes_downloaded);
                let piece_eta = if p.speed > 0.0 {
                    rem as f64 / p.speed
                } else {
                    0.0
                };
                PieceSnapshot {
                    piece_id: p.piece_id.clone(),
                    bytes_downloaded: p.bytes_downloaded,
                    total_bytes: p.total_bytes,
                    speed: p.speed,
                    eta_secs: piece_eta,
                }
            })
            .collect();

        ProgressSnapshot {
            pieces: piece_snapshots,
            total_bytes_downloaded: total_downloaded,
            total_bytes,
            speed: combined_speed,
            eta_secs: eta,
            done: false,
        }
    }

    /// Finalize: build final snapshot with `done = true`, notify all observers.
    async fn finish(self) {
        let elapsed = self.start_time.elapsed();
        let total_downloaded: u64 = self.pieces.values().map(|p| p.bytes_downloaded).sum();
        let avg_speed = if elapsed.as_secs_f64() > 0.0 {
            total_downloaded as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        let mut final_snapshot = self.build_snapshot();
        final_snapshot.done = true;
        final_snapshot.speed = avg_speed;
        final_snapshot.eta_secs = 0.0;

        for observer in &self.observers {
            observer.on_complete(&final_snapshot).await;
        }
    }
}
