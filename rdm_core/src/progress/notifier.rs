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
/// This type is intentionally pure byte-accounting: it has no notion of time,
/// speed, or ETA. Those are wall-clock concerns and belong in the observer
/// layer (e.g. `SseProgressObserver`), which has a stable, low-frequency view
/// of progress that is well-suited to rate measurement.
///
/// # Lifecycle
///
/// | Channel message        | Observer method called         |
/// |------------------------|--------------------------------|
/// | `Ok(ProgressEvent)`    | `on_progress(&snapshot)`       |
/// | `Err(String)`          | `on_error(&msg)` then stops    |
/// | Channel closed (no err)| `on_complete(&final_snapshot)` |
pub struct ProgressNotifier {
    observers: Vec<(usize, Box<dyn ProgressObserver>)>,
    next_observer_id: usize,
    segments: HashMap<String, SegmentProgress>,
    segment_order: Vec<String>,
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

    pub fn add_observer(&mut self, observer: Box<dyn ProgressObserver>) -> usize {
        let id = self.next_observer_id;
        self.next_observer_id += 1;
        self.observers.push((id, observer));
        id
    }

    pub fn remove_observer(&mut self, id: usize) {
        self.observers.retain(|(oid, _)| *oid != id);
    }

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
                    self.agg_total_bytes += tb;
                    segment.total_bytes = tb;
                }
            }
        }

        self.build_snapshot()
    }

    fn build_snapshot(&self) -> ProgressSnapshot {
        let segments = self
            .segment_order
            .iter()
            .filter_map(|id| self.segments.get(id))
            .map(|s| SegmentSnapshot {
                segment_id: s.segment_id.clone(),
                offset: s.offset,
                bytes_downloaded: s.bytes_downloaded,
                total_bytes: s.total_bytes,
            })
            .collect();

        ProgressSnapshot {
            segments,
            total_bytes_downloaded: self.agg_total_downloaded,
            total_bytes: self.agg_total_bytes,
            done: false,
        }
    }

    async fn finish(&self) {
        let mut snap = self.build_snapshot();
        snap.done = true;
        for (_, observer) in &self.observers {
            observer.on_complete(&snap).await;
        }
    }
}
