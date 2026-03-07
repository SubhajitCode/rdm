use std::collections::VecDeque;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::{watch, Mutex};
use rdm_core::progress::observer::ProgressObserver;
use rdm_core::progress::snapshot::ProgressSnapshot;

/// How long to keep samples in the sliding window (seconds).
const WINDOW_SECS: f64 = 3.0;

/// Minimum number of samples required before we report a non-zero speed.
/// Prevents a spurious spike on the very first event.
const MIN_SAMPLES: usize = 2;

/// Internal mutable state for speed measurement.
struct SpeedState {
    /// Ring buffer of (wall_time, cumulative_bytes_downloaded) samples.
    window: VecDeque<(Instant, u64)>,
    /// Wall-clock time the download started (used for the completion average).
    start: Instant,
}

impl SpeedState {
    fn new() -> Self {
        Self {
            window: VecDeque::new(),
            start: Instant::now(),
        }
    }

    /// Record a new sample and evict stale entries outside the window.
    fn push(&mut self, total_downloaded: u64) {
        let now = Instant::now();
        self.window.push_back((now, total_downloaded));

        // Drop samples older than WINDOW_SECS from the front.
        let cutoff = now - std::time::Duration::from_secs_f64(WINDOW_SECS);
        while self.window.len() > MIN_SAMPLES {
            if self.window.front().map_or(false, |(t, _)| *t < cutoff) {
                self.window.pop_front();
            } else {
                break;
            }
        }
    }

    /// Compute the current speed (bytes/sec) over the sliding window.
    fn speed(&self) -> f64 {
        if self.window.len() < MIN_SAMPLES {
            return 0.0;
        }
        let (t0, b0) = self.window.front().unwrap();
        let (t1, b1) = self.window.back().unwrap();
        let dt = t1.duration_since(*t0).as_secs_f64();
        if dt <= 0.0 {
            return 0.0;
        }
        let db = b1.saturating_sub(*b0) as f64;
        db / dt
    }

    /// Compute the average speed over the entire transfer (bytes/sec).
    fn avg_speed(&self, total_downloaded: u64) -> f64 {
        let elapsed = self.start.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        total_downloaded as f64 / elapsed
    }
}

/// Observes download progress, computes speed via a sliding window over real
/// wall time, and pushes the enriched snapshots to a `watch` channel for SSE
/// clients.
///
/// Multiple SSE clients can each hold a clone of the `watch::Receiver` and
/// receive every update in true push fashion — no polling required.
pub struct SseProgressObserver {
    tx: watch::Sender<ProgressSnapshot>,
    state: Mutex<SpeedState>,
}

impl SseProgressObserver {
    /// Creates a new observer and returns both the observer (to be registered
    /// with `ProgressNotifier`) and a `watch::Receiver` that can be cloned
    /// and handed to SSE handler tasks.
    pub fn new() -> (Self, watch::Receiver<ProgressSnapshot>) {
        let (tx, rx) = watch::channel(ProgressSnapshot::empty());
        (Self { tx, state: Mutex::new(SpeedState::new()) }, rx)
    }

    /// Enrich a snapshot with speed + ETA from the sliding window, then send.
    async fn send_with_speed(&self, mut snap: ProgressSnapshot) {
        let mut state = self.state.lock().await;
        state.push(snap.total_bytes_downloaded);
        let speed = state.speed();
        let remaining = snap.total_bytes.saturating_sub(snap.total_bytes_downloaded);
        let eta = if speed > 0.0 { remaining as f64 / speed } else { 0.0 };
        drop(state);

        snap.speed = speed;
        snap.eta_secs = eta;
        let _ = self.tx.send(snap);
    }
}

#[async_trait]
impl ProgressObserver for SseProgressObserver {
    async fn on_progress(&self, snapshot: &ProgressSnapshot) {
        self.send_with_speed(snapshot.clone()).await;
    }

    async fn on_complete(&self, snapshot: &ProgressSnapshot) {
        let mut snap = snapshot.clone();
        // Use the full-transfer average speed for the completion snapshot.
        let state = self.state.lock().await;
        snap.speed = state.avg_speed(snap.total_bytes_downloaded);
        snap.eta_secs = 0.0;
        snap.done = true;
        drop(state);
        let _ = self.tx.send(snap);
    }

    async fn on_error(&self, error: &str) {
        let mut snap = self.tx.borrow().clone();
        snap.done = true;
        log::error!("[SseProgressObserver] download error: {}", error);
        let _ = self.tx.send(snap);
    }
}
