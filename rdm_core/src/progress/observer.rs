use async_trait::async_trait;
use super::snapshot::ProgressSnapshot;

/// Trait for anything that wants to observe download progress.
///
/// The `ProgressNotifier` calls these methods on all registered observers
/// after aggregating raw `ProgressEvent`s into a `ProgressSnapshot`.
///
/// Lifecycle:
/// - `on_progress` is called for every progress event (per-chunk granularity).
/// - `on_complete` is called once when the download finishes successfully
///   (the progress channel closed without an error message).
/// - `on_error` is called once when the download fails (an `Err(String)`
///   was received on the progress channel).
#[async_trait]
pub trait ProgressObserver: Send + Sync + 'static {
    /// Called with the latest aggregated snapshot after each progress event.
    async fn on_progress(&self, snapshot: &ProgressSnapshot);

    /// Called when the download completes successfully.
    async fn on_complete(&self, snapshot: &ProgressSnapshot);

    /// Called when the download fails.
    async fn on_error(&self, error: &str);
}
