use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::{watch, Mutex};

use rdm_core::progress::observer::ProgressObserver;
use rdm_core::progress::snapshot::ProgressSnapshot;

// ── Sliding-window constants ──────────────────────────────────────────────────

/// Width of the sliding window used for speed measurement (seconds).
const WINDOW_SECS: f64 = 3.0;

/// Minimum samples needed before reporting a non-zero speed.
/// Prevents a spurious spike on the very first event.
const MIN_SAMPLES: usize = 2;

// ── Wire types (serialised to JSON and sent over SSE) ─────────────────────────

/// Per-segment slice of the enriched snapshot sent to SSE clients.
#[derive(Debug, Clone, Serialize)]
pub struct EnrichedSegment {
    pub segment_id: String,
    pub offset: u64,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    /// Bytes/sec for this segment over the sliding window.
    pub speed: f64,
    /// Seconds until this segment is expected to complete.
    pub eta_secs: f64,
}

/// Full enriched snapshot sent to SSE clients.
/// This is the only type serialised to JSON on the wire; the core
/// `ProgressSnapshot` never leaves the server process.
#[derive(Debug, Clone, Serialize)]
pub struct EnrichedSnapshot {
    pub segments: Vec<EnrichedSegment>,
    pub total_bytes_downloaded: u64,
    pub total_bytes: u64,
    /// Aggregate bytes/sec over the sliding window (sum of segment speeds).
    pub speed: f64,
    /// Seconds until the whole download completes at current speed.
    pub eta_secs: f64,
    pub done: bool,
}

impl EnrichedSnapshot {
    fn empty() -> Self {
        Self {
            segments: Vec::new(),
            total_bytes_downloaded: 0,
            total_bytes: 0,
            speed: 0.0,
            eta_secs: 0.0,
            done: false,
        }
    }
}

// ── Sliding-window tracker ────────────────────────────────────────────────────

/// Tracks speed for a single logical stream (either aggregate or one segment)
/// using a VecDeque ring-buffer of `(wall_time, cumulative_bytes)` samples.
struct Window {
    samples: VecDeque<(Instant, u64)>,
    start: Instant,
}

impl Window {
    fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            start: Instant::now(),
        }
    }

    /// Record the current cumulative byte count and evict stale samples.
    fn push(&mut self, cumulative_bytes: u64) {
        let now = Instant::now();
        self.samples.push_back((now, cumulative_bytes));

        let cutoff = now - std::time::Duration::from_secs_f64(WINDOW_SECS);
        // Always keep at least MIN_SAMPLES so the window is never empty.
        while self.samples.len() > MIN_SAMPLES {
            if self.samples.front().map_or(false, |(t, _)| *t < cutoff) {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Current speed (bytes/sec) over the window. Returns 0 until MIN_SAMPLES
    /// have been collected.
    fn speed(&self) -> f64 {
        if self.samples.len() < MIN_SAMPLES {
            return 0.0;
        }
        let (t0, b0) = self.samples.front().unwrap();
        let (t1, b1) = self.samples.back().unwrap();
        let dt = t1.duration_since(*t0).as_secs_f64();
        if dt <= 0.0 {
            return 0.0;
        }
        b1.saturating_sub(*b0) as f64 / dt
    }

    /// Average speed over the entire transfer (bytes/sec). Used for the final
    /// completion snapshot where the sliding window may have gone stale.
    fn avg_speed(&self, total_bytes: u64) -> f64 {
        let elapsed = self.start.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        total_bytes as f64 / elapsed
    }
}

// ── Internal mutable state ────────────────────────────────────────────────────

struct SpeedState {
    /// Aggregate window — tracks total_bytes_downloaded across all segments.
    aggregate: Window,
    /// Per-segment windows — keyed by segment_id.
    segments: HashMap<String, Window>,
}

impl SpeedState {
    fn new() -> Self {
        Self {
            aggregate: Window::new(),
            segments: HashMap::new(),
        }
    }

    /// Update windows from a fresh `ProgressSnapshot` and produce an
    /// `EnrichedSnapshot` with speed + ETA filled in.
    fn enrich(&mut self, snap: &ProgressSnapshot, done: bool) -> EnrichedSnapshot {
        // Update aggregate window.
        self.aggregate.push(snap.total_bytes_downloaded);
        let agg_speed = self.aggregate.speed();
        let agg_remaining = snap.total_bytes.saturating_sub(snap.total_bytes_downloaded);
        let agg_eta = if agg_speed > 0.0 { agg_remaining as f64 / agg_speed } else { 0.0 };

        // Update per-segment windows and build enriched segments.
        let segments = snap
            .segments
            .iter()
            .map(|s| {
                let win = self.segments
                    .entry(s.segment_id.clone())
                    .or_insert_with(Window::new);
                win.push(s.bytes_downloaded);
                let speed = win.speed();
                let remaining = s.total_bytes.saturating_sub(s.bytes_downloaded);
                let eta = if speed > 0.0 { remaining as f64 / speed } else { 0.0 };
                EnrichedSegment {
                    segment_id: s.segment_id.clone(),
                    offset: s.offset,
                    bytes_downloaded: s.bytes_downloaded,
                    total_bytes: s.total_bytes,
                    speed,
                    eta_secs: eta,
                }
            })
            .collect();

        EnrichedSnapshot {
            segments,
            total_bytes_downloaded: snap.total_bytes_downloaded,
            total_bytes: snap.total_bytes,
            speed: agg_speed,
            eta_secs: agg_eta,
            done,
        }
    }

    /// Produce the final completion snapshot using wall-clock averages
    /// (the sliding window may have gone stale by the time the download ends).
    fn enrich_final(&mut self, snap: &ProgressSnapshot) -> EnrichedSnapshot {
        let avg_speed = self.aggregate.avg_speed(snap.total_bytes_downloaded);

        let segments = snap
            .segments
            .iter()
            .map(|s| {
                let win = self.segments
                    .entry(s.segment_id.clone())
                    .or_insert_with(Window::new);
                let seg_avg = win.avg_speed(s.bytes_downloaded);
                EnrichedSegment {
                    segment_id: s.segment_id.clone(),
                    offset: s.offset,
                    bytes_downloaded: s.bytes_downloaded,
                    total_bytes: s.total_bytes,
                    speed: seg_avg,
                    eta_secs: 0.0,
                }
            })
            .collect();

        EnrichedSnapshot {
            segments,
            total_bytes_downloaded: snap.total_bytes_downloaded,
            total_bytes: snap.total_bytes,
            speed: avg_speed,
            eta_secs: 0.0,
            done: true,
        }
    }
}

// ── Observer ──────────────────────────────────────────────────────────────────

/// Observes raw byte-count snapshots from `ProgressNotifier`, computes speed
/// and ETA via per-segment and aggregate sliding windows over real wall time,
/// and pushes fully-enriched `EnrichedSnapshot`s to a `watch` channel for SSE
/// clients.
pub struct SseProgressObserver {
    tx: watch::Sender<EnrichedSnapshot>,
    state: Mutex<SpeedState>,
}

impl SseProgressObserver {
    pub fn new() -> (Self, watch::Receiver<EnrichedSnapshot>) {
        let (tx, rx) = watch::channel(EnrichedSnapshot::empty());
        (Self { tx, state: Mutex::new(SpeedState::new()) }, rx)
    }
}

#[async_trait]
impl ProgressObserver for SseProgressObserver {
    async fn on_progress(&self, snapshot: &ProgressSnapshot) {
        let enriched = self.state.lock().await.enrich(snapshot, false);
        let _ = self.tx.send(enriched);
    }

    async fn on_complete(&self, snapshot: &ProgressSnapshot) {
        let enriched = self.state.lock().await.enrich_final(snapshot);
        let _ = self.tx.send(enriched);
    }

    async fn on_error(&self, error: &str) {
        let mut snap = self.tx.borrow().clone();
        snap.done = true;
        log::error!("[SseProgressObserver] download error: {}", error);
        let _ = self.tx.send(snap);
    }
}
