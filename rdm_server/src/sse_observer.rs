use async_trait::async_trait;
use tokio::sync::watch;
use rdm_core::progress::observer::ProgressObserver;
use rdm_core::progress::snapshot::ProgressSnapshot;

/// Observes download progress and pushes snapshots to a `watch` channel
/// so that SSE clients can receive them via `rx.changed().await`.
///
/// Multiple SSE clients can each hold a clone of the `watch::Receiver` and
/// receive every update in true push fashion — no polling required.
pub struct SseProgressObserver {
    tx: watch::Sender<ProgressSnapshot>,
}

impl SseProgressObserver {
    /// Creates a new observer and returns both the observer (to be registered
    /// with `ProgressNotifier`) and a `watch::Receiver` that can be cloned
    /// and handed to SSE handler tasks.
    pub fn new() -> (Self, watch::Receiver<ProgressSnapshot>) {
        let (tx, rx) = watch::channel(ProgressSnapshot::empty());
        (Self { tx }, rx)
    }
}

#[async_trait]
impl ProgressObserver for SseProgressObserver {
    async fn on_progress(&self, snapshot: &ProgressSnapshot) {
        // send() only fails if all receivers are dropped; we can safely ignore that.
        let _ = self.tx.send(snapshot.clone());
    }

    async fn on_complete(&self, snapshot: &ProgressSnapshot) {
        let _ = self.tx.send(snapshot.clone());
    }

    async fn on_error(&self, error: &str) {
        let mut snap = self.tx.borrow().clone();
        snap.done = true;
        // Embed the error string in the message field isn't ideal, but
        // ProgressSnapshot doesn't have an error field yet.  We mark done=true
        // so the SSE stream closes, and log the error server-side.
        log::error!("[SseProgressObserver] download error: {}", error);
        let _ = self.tx.send(snap);
    }
}
