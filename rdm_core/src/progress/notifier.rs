use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::types::types::ProgressEvent;
use super::observer::ProgressObserver;
use super::snapshot::{SegmentSnapshot, ProgressSnapshot};

struct SegmentProgress {
    segment_id: String,
    offset: u64,
    bytes_downloaded: u64,
    total_bytes: u64,
}

/// Consumes `Result<ProgressEvent, String>` from the download channel,
/// aggregates byte counts into `ProgressSnapshot`s, and fans out to all
/// registered observers.
///
/// Speed and ETA are intentionally **not** computed here — they depend on
/// wall-clock time and belong in the observer layer (e.g. `SseProgressObserver`),
/// which has a stable, low-frequency view of progress suited for rate measurement.
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
    /// Incrementally maintained aggregates — avoids O(n) iteration on every event.
    agg_total_bytes: u64,
    agg_total_downloaded: u64,
}

impl ProgressNotifier {
    pub fn new() -> Self {
        Self {
            observers: Vec::new(),
            next_observer_id: 0,
            segments: HashMap::new(),
            segment_order: Vec::new(),
            agg_total_bytes: 0,
            agg_total_downloaded: 0,
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
        if !self.segments.contains_key(&ev.segment_id) {
            let total = ev.total_bytes.unwrap_or(0);
            self.segment_order.push(ev.segment_id.clone());
            self.segments.insert(
                ev.segment_id.clone(),
                SegmentProgress {
                    segment_id: ev.segment_id.clone(),
                    offset: ev.offset,
                    bytes_downloaded: 0,
                    total_bytes: total,
                },
            );
            self.agg_total_bytes += total;
        }

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
        }

        self.build_snapshot()
    }

    fn build_snapshot(&self) -> ProgressSnapshot {
        let segment_snapshots: Vec<SegmentSnapshot> = self
            .segment_order
            .iter()
            .filter_map(|id| self.segments.get(id))
            .map(|s| SegmentSnapshot {
                segment_id: s.segment_id.clone(),
                offset: s.offset,
                bytes_downloaded: s.bytes_downloaded,
                total_bytes: s.total_bytes,
                // Speed and ETA are filled in by the observer layer.
                speed: 0.0,
                eta_secs: 0.0,
            })
            .collect();

        ProgressSnapshot {
            segments: segment_snapshots,
            total_bytes_downloaded: self.agg_total_downloaded,
            total_bytes: self.agg_total_bytes,
            // Speed and ETA are filled in by the observer layer.
            speed: 0.0,
            eta_secs: 0.0,
            done: false,
        }
    }

    async fn finish(&self) {
        let mut final_snapshot = self.build_snapshot();
        final_snapshot.done = true;
        for (_, observer) in &self.observers {
            observer.on_complete(&final_snapshot).await;
        }
    }
}
