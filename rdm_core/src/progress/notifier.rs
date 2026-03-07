use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::types::types::ProgressEvent;
use super::observer::ProgressObserver;
use super::snapshot::{SegmentSnapshot, ProgressSnapshot};

/// EMA smoothing factor. 0.3 = responsive but stable.
const EMA_ALPHA: f64 = 0.3;

struct SegmentProgress {
    segment_id: String,
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
    /// Observers stored with a stable ID so they can be deregistered.
    observers: Vec<(usize, Box<dyn ProgressObserver>)>,
    next_observer_id: usize,
    segments: HashMap<String, SegmentProgress>,
    segment_order: Vec<String>,
    start_time: Instant,
    /// Incrementally maintained aggregates — avoids O(n) iteration on every event.
    agg_total_bytes: u64,
    agg_total_downloaded: u64,
    agg_combined_speed: f64,
}

impl ProgressNotifier {
    pub fn new() -> Self {
        Self {
            observers: Vec::new(),
            next_observer_id: 0,
            segments: HashMap::new(),
            segment_order: Vec::new(),
            start_time: Instant::now(),
            agg_total_bytes: 0,
            agg_total_downloaded: 0,
            agg_combined_speed: 0.0,
        }
    }

    /// Register an observer and return its ID (use with `remove_observer`).
    /// Must be called before `run()`.
    pub fn add_observer(&mut self, observer: Box<dyn ProgressObserver>) -> usize {
        let id = self.next_observer_id;
        self.next_observer_id += 1;
        self.observers.push((id, observer));
        id
    }

    /// Unregister an observer by the ID returned from `add_observer`.
    /// No-op if the ID is not found.
    pub fn remove_observer(&mut self, id: usize) {
        self.observers.retain(|(oid, _)| *oid != id);
    }

    /// Consume progress messages until the channel closes or an error arrives.
    pub async fn run(
        &mut self,
        mut progress_rx: mpsc::Receiver<Result<ProgressEvent, String>>,
    ) {
        while let Some(msg) = progress_rx.recv().await {
            match msg {
                Ok(ev) => {
                    let snapshot = self.handle_event(ev);
                    for (_, observer) in &self.observers {
                        observer.on_progress(&snapshot).await;
                    }
                }
                Err(error) => {
                    for (_, observer) in &self.observers {
                        observer.on_error(&error).await;
                    }
                    return;
                }
            }
        }
        self.finish().await;
    }

    fn handle_event(&mut self, ev: ProgressEvent) -> ProgressSnapshot {
        let now = Instant::now();

        if !self.segments.contains_key(&ev.segment_id) {
            let total = ev.total_bytes.unwrap_or(0);
            self.segment_order.push(ev.segment_id.clone());
            self.segments.insert(
                ev.segment_id.clone(),
                SegmentProgress {
                    segment_id: ev.segment_id.clone(),
                    bytes_downloaded: 0,
                    total_bytes: total,
                    speed: 0.0,
                    last_update: now,
                },
            );
            // New segment: add its total to the aggregate.
            self.agg_total_bytes += total;
        }

        // Accumulate downloaded bytes into the aggregate.
        self.agg_total_downloaded += ev.bytes_delta;

        {
            let segment = self.segments.get_mut(&ev.segment_id).unwrap();
            segment.bytes_downloaded += ev.bytes_delta;

            if segment.total_bytes == 0 {
                if let Some(tb) = ev.total_bytes {
                    // Segment total was unknown at registration; update aggregate now.
                    self.agg_total_bytes += tb;
                    segment.total_bytes = tb;
                }
            }

            let elapsed = now.duration_since(segment.last_update).as_secs_f64();
            if elapsed > 0.0 {
                let instant_speed = ev.bytes_delta as f64 / elapsed;
                let old_speed = segment.speed;
                let new_speed = EMA_ALPHA * instant_speed + (1.0 - EMA_ALPHA) * old_speed;
                // Update combined speed incrementally: subtract old, add new.
                self.agg_combined_speed = (self.agg_combined_speed - old_speed + new_speed).max(0.0);
                segment.speed = new_speed;
                segment.last_update = now;
            }
        }

        self.build_snapshot()
    }

    fn build_snapshot(&self) -> ProgressSnapshot {
        // Aggregate totals are O(1) thanks to incremental maintenance.
        let total_bytes = self.agg_total_bytes;
        let total_downloaded = self.agg_total_downloaded;
        let combined_speed = self.agg_combined_speed;

        let remaining = total_bytes.saturating_sub(total_downloaded);
        let eta = if combined_speed > 0.0 {
            remaining as f64 / combined_speed
        } else {
            0.0
        };

        // Per-segment snapshots are inherently O(n) but represent a small, fixed-size list.
        let segment_snapshots: Vec<SegmentSnapshot> = self
            .segment_order
            .iter()
            .filter_map(|id| self.segments.get(id))
            .map(|s| {
                let rem = s.total_bytes.saturating_sub(s.bytes_downloaded);
                let segment_eta = if s.speed > 0.0 {
                    rem as f64 / s.speed
                } else {
                    0.0
                };
                SegmentSnapshot {
                    segment_id: s.segment_id.clone(),
                    bytes_downloaded: s.bytes_downloaded,
                    total_bytes: s.total_bytes,
                    speed: s.speed,
                    eta_secs: segment_eta,
                }
            })
            .collect();

        ProgressSnapshot {
            segments: segment_snapshots,
            total_bytes_downloaded: total_downloaded,
            total_bytes,
            speed: combined_speed,
            eta_secs: eta,
            done: false,
        }
    }

    async fn finish(&self) {
        let elapsed = self.start_time.elapsed();
        let avg_speed = if elapsed.as_secs_f64() > 0.0 {
            self.agg_total_downloaded as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        let mut final_snapshot = self.build_snapshot();
        final_snapshot.done = true;
        final_snapshot.speed = avg_speed;
        final_snapshot.eta_secs = 0.0;

        for (_, observer) in &self.observers {
            observer.on_complete(&final_snapshot).await;
        }
    }
}
