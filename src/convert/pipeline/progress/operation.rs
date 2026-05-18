//! Route-neutral operation progress tracking.

use std::time::{Duration, Instant};

use super::confidence::{ProgressConfidence, ProgressScope};
use super::elapsed::{append_elapsed, DEFAULT_ELAPSED_THRESHOLD};
use super::throttle::ProgressThrottle;
use crate::convert::pipeline::reporter::{PipelineEvent, PipelineReporter};
use crate::convert::pipeline::types::PipelineStage;

#[derive(Debug, Clone)]
struct OperationProgressState {
    last_progress: f32,
    last_confidence: ProgressConfidence,
    last_scope: ProgressScope,
}

impl Default for OperationProgressState {
    fn default() -> Self {
        Self {
            last_progress: 0.0,
            last_confidence: ProgressConfidence::Unknown,
            last_scope: ProgressScope::Stage,
        }
    }
}

/// Emits route-neutral `PipelineEvent::Progress` updates for long-running work.
///
/// The tracker sits close to stage code and child-process wrappers. It performs
/// source-side coalescing before events reach `BroadcastReporter`, which keeps
/// future tool-output probes from flooding the TUI path.
pub struct OperationProgressTracker<'a> {
    reporter: Option<&'a dyn PipelineReporter>,
    item_id: String,
    stage: PipelineStage,
    started_at: Instant,
    elapsed_threshold: Duration,
    throttle: ProgressThrottle,
    state: OperationProgressState,
}

impl<'a> OperationProgressTracker<'a> {
    pub fn new(
        item_id: impl Into<String>,
        stage: PipelineStage,
        reporter: Option<&'a dyn PipelineReporter>,
    ) -> Self {
        Self {
            reporter,
            item_id: item_id.into(),
            stage,
            started_at: Instant::now(),
            elapsed_threshold: DEFAULT_ELAPSED_THRESHOLD,
            throttle: ProgressThrottle::default(),
            state: OperationProgressState::default(),
        }
    }

    pub fn with_elapsed_threshold(mut self, threshold: Duration) -> Self {
        self.elapsed_threshold = threshold;
        self
    }

    /// Start a logical unit of work such as a file or track.
    pub async fn start_unit(&mut self, ordinal: usize, total: usize, name: impl AsRef<str>) {
        let name = name.as_ref();
        let key = format!("unit:{ordinal}:{total}");
        self.emit_progress_lazy(
            self.state.last_progress,
            &key,
            || format!("Starting unit {ordinal} of {total}: {name}"),
            ProgressConfidence::Unknown,
            ProgressScope::Track,
            true,
        )
        .await;
    }

    /// Emit measured progress from a reliable denominator.
    ///
    /// This method does not allocate a new `String` before throttling rejects an
    /// update when the caller passes a borrowed string. For volatile tool output,
    /// prefer `measured_with_key_lazy` so changing display text does not count as
    /// a material message change and expensive formatting can be skipped.
    pub async fn measured(&mut self, progress: f32, message: impl AsRef<str>) {
        let message = message.as_ref();
        self.measured_with_key_lazy(progress, message, || message.to_string())
            .await;
    }

    /// Emit measured progress with a stable material key and borrowed display
    /// message. The key, not the display text, drives message-change throttling.
    pub async fn measured_with_key(
        &mut self,
        progress: f32,
        material_key: impl AsRef<str>,
        message: impl AsRef<str>,
    ) {
        let material_key = material_key.as_ref();
        let message = message.as_ref();
        self.measured_with_key_lazy(progress, material_key, || message.to_string())
            .await;
    }

    /// Emit measured progress with lazy message construction. Use this for
    /// high-frequency tool probes that would otherwise allocate formatted
    /// strings even when the update is coalesced.
    pub async fn measured_with_key_lazy(
        &mut self,
        progress: f32,
        material_key: impl AsRef<str>,
        build_message: impl FnOnce() -> String,
    ) {
        self.emit_progress_lazy(
            progress,
            material_key.as_ref(),
            build_message,
            ProgressConfidence::Measured,
            ProgressScope::Tool,
            false,
        )
        .await;
    }

    /// Emit estimated progress from samples, duration, or item-count modeling.
    pub async fn estimated(&mut self, progress: f32, message: impl AsRef<str>) {
        let message = message.as_ref();
        self.estimated_with_key_lazy(progress, message, || message.to_string())
            .await;
    }

    /// Emit estimated progress with a stable material key and borrowed display
    /// message. The key, not the display text, drives message-change throttling.
    pub async fn estimated_with_key(
        &mut self,
        progress: f32,
        material_key: impl AsRef<str>,
        message: impl AsRef<str>,
    ) {
        let material_key = material_key.as_ref();
        let message = message.as_ref();
        self.estimated_with_key_lazy(progress, material_key, || message.to_string())
            .await;
    }

    /// Emit estimated progress with lazy message construction. Use this for
    /// high-frequency probes or polling loops.
    pub async fn estimated_with_key_lazy(
        &mut self,
        progress: f32,
        material_key: impl AsRef<str>,
        build_message: impl FnOnce() -> String,
    ) {
        self.emit_progress_lazy(
            progress,
            material_key.as_ref(),
            build_message,
            ProgressConfidence::Estimated,
            ProgressScope::Stage,
            false,
        )
        .await;
    }

    /// Emit a liveness update without moving progress forward.
    pub async fn unknown_alive(&mut self, message: impl AsRef<str>) {
        let message = message.as_ref();
        self.unknown_alive_with_key_lazy(message, || message.to_string())
            .await;
    }

    /// Emit a liveness update with a stable material key and borrowed display
    /// message. The key, not the display text, drives message-change throttling.
    pub async fn unknown_alive_with_key(
        &mut self,
        material_key: impl AsRef<str>,
        message: impl AsRef<str>,
    ) {
        let material_key = material_key.as_ref();
        let message = message.as_ref();
        self.unknown_alive_with_key_lazy(material_key, || message.to_string())
            .await;
    }

    /// Emit a liveness update with lazy message construction.
    pub async fn unknown_alive_with_key_lazy(
        &mut self,
        material_key: impl AsRef<str>,
        build_message: impl FnOnce() -> String,
    ) {
        self.emit_progress_lazy(
            self.state.last_progress,
            material_key.as_ref(),
            build_message,
            ProgressConfidence::Unknown,
            ProgressScope::Stage,
            false,
        )
        .await;
    }

    /// Finish a logical unit of work using simple item-count progress.
    pub async fn finish_unit(&mut self, ordinal: usize, total: usize, name: impl AsRef<str>) {
        let progress = if total == 0 {
            self.state.last_progress
        } else {
            ordinal as f32 / total as f32
        };
        let name = name.as_ref();
        let key = format!("unit:{ordinal}:{total}:finished");
        self.emit_progress_lazy(
            progress,
            &key,
            || format!("Finished unit {ordinal} of {total}: {name}"),
            ProgressConfidence::Estimated,
            ProgressScope::Track,
            true,
        )
        .await;
    }

    /// Emit immediate failure visibility without changing terminal state. The
    /// orchestrator should still emit the real terminal event through the
    /// pipeline reporter when the job or stage ends.
    pub async fn failure(&mut self, message: impl AsRef<str>) {
        let message = message.as_ref();
        self.emit_progress_lazy(
            self.state.last_progress,
            "failure",
            || message.to_string(),
            ProgressConfidence::Unknown,
            ProgressScope::Stage,
            true,
        )
        .await;
    }

    /// Emit immediate cancellation visibility. This does not perform process
    /// cancellation; it only reports that cancellation has been requested.
    pub async fn cancel_requested(&mut self) {
        self.emit_progress_lazy(
            self.state.last_progress,
            "cancellation",
            || "Cancelling…".to_string(),
            ProgressConfidence::Unknown,
            ProgressScope::Stage,
            true,
        )
        .await;
    }

    pub fn last_progress(&self) -> f32 {
        self.state.last_progress
    }

    pub fn last_confidence(&self) -> ProgressConfidence {
        self.state.last_confidence
    }

    pub fn last_scope(&self) -> ProgressScope {
        self.state.last_scope
    }

    async fn emit_progress_lazy(
        &mut self,
        progress: f32,
        material_key: &str,
        build_message: impl FnOnce() -> String,
        confidence: ProgressConfidence,
        scope: ProgressScope,
        force: bool,
    ) {
        let progress = progress.clamp(0.0, 1.0).max(self.state.last_progress);
        let now = Instant::now();
        if !self
            .throttle
            .should_send(progress, material_key, force, now)
        {
            return;
        }

        self.state.last_progress = progress;
        self.state.last_confidence = confidence;
        self.state.last_scope = scope;

        let material_message = build_message();
        let message = append_elapsed(
            &material_message,
            now.duration_since(self.started_at),
            self.elapsed_threshold,
        );

        if let Some(reporter) = self.reporter {
            reporter
                .emit(PipelineEvent::Progress {
                    item_id: self.item_id.clone(),
                    stage: self.stage,
                    phase_progress: progress,
                    message: Some(message),
                })
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::pipeline::reporter::RecordingReporter;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn progress_events(reporter: &RecordingReporter) -> Vec<PipelineEvent> {
        reporter
            .events()
            .into_iter()
            .filter(|event| matches!(event, PipelineEvent::Progress { .. }))
            .collect()
    }

    fn progress_values(reporter: &RecordingReporter) -> Vec<f32> {
        progress_events(reporter)
            .into_iter()
            .map(|event| match event {
                PipelineEvent::Progress { phase_progress, .. } => phase_progress,
                _ => unreachable!(),
            })
            .collect()
    }

    fn progress_messages(reporter: &RecordingReporter) -> Vec<String> {
        progress_events(reporter)
            .into_iter()
            .map(|event| match event {
                PipelineEvent::Progress { message, .. } => message.expect("message"),
                _ => unreachable!(),
            })
            .collect()
    }

    #[tokio::test]
    async fn start_and_finish_unit_emit_messages() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.start_unit(1, 2, "So What").await;
        tracker.finish_unit(1, 2, "So What").await;

        let messages = progress_messages(&reporter);
        assert_eq!(messages[0], "Starting unit 1 of 2: So What");
        assert_eq!(messages[1], "Finished unit 1 of 2: So What");
        assert_eq!(tracker.last_scope(), ProgressScope::Track);
    }

    #[tokio::test]
    async fn high_frequency_same_key_updates_are_coalesced() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.estimated(0.100, "Converting").await;
        tracker.estimated(0.102, "Converting").await;

        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn material_key_change_emits_immediately() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker
            .estimated_with_key(0.100, "track-1", "Converting track 1")
            .await;
        tracker
            .estimated_with_key(0.101, "track-2", "Converting track 2")
            .await;

        assert_eq!(progress_events(&reporter).len(), 2);
    }

    #[tokio::test]
    async fn volatile_display_message_does_not_bypass_stable_key_throttle() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker
            .measured_with_key(0.100, "ffmpeg-track-1", "frame 1 speed 1.0x")
            .await;
        tracker
            .measured_with_key(0.101, "ffmpeg-track-1", "frame 2 speed 1.1x")
            .await;

        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn lazy_message_builder_is_not_called_for_suppressed_updates() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );
        let builds = Arc::new(AtomicUsize::new(0));

        let first_builds = Arc::clone(&builds);
        tracker
            .measured_with_key_lazy(0.100, "tool-progress", || {
                first_builds.fetch_add(1, Ordering::SeqCst);
                "tool progress 1".to_string()
            })
            .await;

        let second_builds = Arc::clone(&builds);
        tracker
            .measured_with_key_lazy(0.101, "tool-progress", || {
                second_builds.fetch_add(1, Ordering::SeqCst);
                "tool progress 2".to_string()
            })
            .await;

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn progress_delta_emits_immediately() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.estimated(0.100, "Converting").await;
        tracker.estimated(0.106, "Converting").await;

        assert_eq!(progress_values(&reporter), vec![0.100, 0.106]);
    }

    #[tokio::test]
    async fn progress_decrease_is_monotonic_when_message_changes() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.estimated(0.400, "Converting track 1").await;
        tracker.estimated(0.200, "Late duplicate for track 1").await;

        assert_eq!(progress_values(&reporter), vec![0.400, 0.400]);
        assert_eq!(tracker.last_progress(), 0.400);
    }

    #[tokio::test]
    async fn unknown_alive_does_not_advance_progress() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.estimated(0.400, "Converting").await;
        tracker.unknown_alive("Still running").await;

        assert_eq!(progress_values(&reporter), vec![0.400, 0.400]);
        assert_eq!(tracker.last_confidence(), ProgressConfidence::Unknown);
    }

    #[tokio::test]
    async fn failure_bypasses_throttle_and_preserves_progress() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.estimated(0.250, "Converting").await;
        tracker.failure("Encoder failed").await;

        assert_eq!(progress_values(&reporter), vec![0.250, 0.250]);
        assert_eq!(progress_messages(&reporter), vec!["Converting", "Encoder failed"]);
    }

    #[tokio::test]
    async fn cancel_requested_bypasses_throttle() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        );

        tracker.estimated(0.250, "Converting").await;
        tracker.cancel_requested().await;

        let messages = progress_messages(&reporter);
        assert_eq!(messages, vec!["Converting", "Cancelling…"]);
    }

    #[tokio::test]
    async fn elapsed_text_appears_after_threshold() {
        let reporter = RecordingReporter::new();
        let mut tracker = OperationProgressTracker::new(
            "item-1",
            PipelineStage::Convert,
            Some(&reporter),
        )
        .with_elapsed_threshold(Duration::from_secs(0));

        tracker.estimated(0.100, "Converting").await;

        let messages = progress_messages(&reporter);
        assert!(messages[0].starts_with("Converting · elapsed "));
    }
}
