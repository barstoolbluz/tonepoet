//! Route-neutral operation progress tracking.

use std::time::{Duration, Instant};

use super::confidence::{ProgressConfidence, ProgressScope};
use super::elapsed::{append_elapsed, DEFAULT_ELAPSED_THRESHOLD};
use super::eta::{append_eta, EtaEstimator};
use super::throttle::ProgressThrottle;
use crate::convert::pipeline::reporter::{PipelineEvent, PipelineReporter};
use crate::convert::pipeline::types::PipelineStage;

#[derive(Debug, Clone)]
struct OperationProgressState {
    last_progress: f32,
    last_confidence: ProgressConfidence,
    last_scope: ProgressScope,
    terminal_visibility_locked: bool,
}

impl Default for OperationProgressState {
    fn default() -> Self {
        Self {
            last_progress: 0.0,
            last_confidence: ProgressConfidence::Unknown,
            last_scope: ProgressScope::Stage,
            terminal_visibility_locked: false,
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
    eta: EtaEstimator,
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
            eta: EtaEstimator::default(),
            state: OperationProgressState::default(),
        }
    }

    pub fn with_elapsed_threshold(mut self, threshold: Duration) -> Self {
        self.elapsed_threshold = threshold;
        self
    }

    /// Start a logical unit of work such as a file or track.
    pub async fn start_unit(&mut self, ordinal: usize, total: usize, name: impl AsRef<str>) {
        self.start_unit_with_weight(ordinal, total, name, None, None)
            .await;
    }

    /// Start a logical unit with a duration/sample weight and the total weight
    /// for the operation.
    pub async fn start_weighted_unit(
        &mut self,
        ordinal: usize,
        total: usize,
        name: impl AsRef<str>,
        unit_weight: u64,
        total_weight: u64,
    ) {
        self.start_unit_with_weight(
            ordinal,
            total,
            name,
            Some(unit_weight as f64),
            Some(total_weight as f64),
        )
        .await;
    }

    /// Record the start of a logical unit without emitting a progress event.
    ///
    /// Use this when existing stage code already emits user-facing messages but
    /// the ETA model still needs real unit boundaries. With no weights, the ETA
    /// model falls back to item count.
    pub fn observe_unit_start(&mut self, ordinal: usize, total: usize) {
        self.observe_weighted_unit_start(ordinal, total, None, None);
    }

    /// Record the start of a logical unit with an optional duration/sample
    /// weight and optional total operation weight, without emitting.
    pub fn observe_weighted_unit_start(
        &mut self,
        ordinal: usize,
        total: usize,
        unit_weight: Option<u64>,
        total_weight: Option<u64>,
    ) {
        self.eta.start_unit(
            ordinal,
            total,
            unit_weight.map(|weight| weight as f64),
            total_weight.map(|weight| weight as f64),
            Instant::now(),
        );
    }

    /// Permanently suppress ETA for this operation.
    ///
    /// Call this before failure, skip, cancellation, or any other path where the
    /// remaining-work model is no longer trustworthy.
    pub fn suppress_eta(&mut self) {
        self.eta.suppress();
    }

    async fn start_unit_with_weight(
        &mut self,
        ordinal: usize,
        total: usize,
        name: impl AsRef<str>,
        unit_weight: Option<f64>,
        total_weight: Option<f64>,
    ) {
        self.eta
            .start_unit(ordinal, total, unit_weight, total_weight, Instant::now());

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
        self.finish_unit_with_weight(ordinal, total, name, None, None)
            .await;
    }

    /// Finish a logical unit with a duration/sample weight and the total weight
    /// for the operation.
    pub async fn finish_weighted_unit(
        &mut self,
        ordinal: usize,
        total: usize,
        name: impl AsRef<str>,
        unit_weight: u64,
        total_weight: u64,
    ) {
        self.finish_unit_with_weight(
            ordinal,
            total,
            name,
            Some(unit_weight as f64),
            Some(total_weight as f64),
        )
        .await;
    }

    /// Record completion of a logical unit without emitting a progress event.
    pub fn observe_unit_finish(&mut self, ordinal: usize, total: usize) {
        self.observe_weighted_unit_finish(ordinal, total, None, None);
    }

    /// Record completion of a logical unit with an optional duration/sample
    /// weight and optional total operation weight, without emitting.
    pub fn observe_weighted_unit_finish(
        &mut self,
        ordinal: usize,
        total: usize,
        unit_weight: Option<u64>,
        total_weight: Option<u64>,
    ) {
        self.eta.finish_unit(
            ordinal,
            total,
            unit_weight.map(|weight| weight as f64),
            total_weight.map(|weight| weight as f64),
            Instant::now(),
        );
    }

    async fn finish_unit_with_weight(
        &mut self,
        ordinal: usize,
        total: usize,
        name: impl AsRef<str>,
        unit_weight: Option<f64>,
        total_weight: Option<f64>,
    ) {
        self.eta
            .finish_unit(ordinal, total, unit_weight, total_weight, Instant::now());

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
        self.eta.suppress();
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
        self.eta.suppress();
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

    /// Emit immediate cancellation visibility for a specific external tool.
    pub async fn cancel_requested_for_tool(&mut self, tool_name: impl AsRef<str>) {
        self.eta.suppress();
        let tool_name = tool_name.as_ref();
        self.emit_progress_lazy(
            self.state.last_progress,
            "tool-cancellation",
            || format!("Stopping {tool_name}…"),
            ProgressConfidence::Unknown,
            ProgressScope::Tool,
            true,
        )
        .await;
    }

    /// Emit a final progress-point message for cancellation-aware renderers.
    pub async fn cancelled_at_last_progress(&mut self) {
        self.eta.suppress();
        if self.state.terminal_visibility_locked {
            return;
        }
        let message = cancelled_at_message(self.state.last_progress);
        self.emit_progress_lazy(
            self.state.last_progress,
            "cancelled-at-progress",
            || message,
            ProgressConfidence::Unknown,
            ProgressScope::Stage,
            true,
        )
        .await;
        self.state.terminal_visibility_locked = true;
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

    #[cfg(test)]
    fn force_active_eta_unit_elapsed_for_tests(&mut self, elapsed: Duration) {
        self.eta
            .force_active_unit_elapsed_for_tests(elapsed, Instant::now());
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
        if self.state.terminal_visibility_locked {
            return;
        }

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
        let eta = self.eta_for_update(progress, confidence, scope, now);
        let message_with_eta = append_eta(&material_message, eta);
        let message = append_elapsed(
            &message_with_eta,
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

    fn eta_for_update(
        &self,
        progress: f32,
        confidence: ProgressConfidence,
        scope: ProgressScope,
        now: Instant,
    ) -> Option<Duration> {
        match (confidence, scope) {
            (ProgressConfidence::Measured, ProgressScope::Tool) => self
                .eta
                .remaining_from_measured_progress(progress, now.duration_since(self.started_at))
                .or_else(|| self.eta.remaining_from_completed_units()),
            (ProgressConfidence::Estimated, _) => self.eta.remaining_from_completed_units(),
            _ => None,
        }
    }
}

fn cancelled_at_message(progress: f32) -> String {
    let percent = ((progress.clamp(0.0, 1.0) * 100.0).round() as u8).min(100);
    if percent == 0 {
        "Cancelled".to_string()
    } else {
        format!("Cancelled at {percent}%")
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
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
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
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.100, "Converting").await;
        tracker.estimated(0.102, "Converting").await;
        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn material_key_change_emits_immediately() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
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
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
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
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
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
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.100, "Converting").await;
        tracker.estimated(0.106, "Converting").await;
        assert_eq!(progress_values(&reporter), vec![0.100, 0.106]);
    }

    #[tokio::test]
    async fn progress_decrease_is_monotonic_when_message_changes() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.400, "Converting track 1").await;
        tracker.estimated(0.200, "Late duplicate for track 1").await;
        assert_eq!(progress_values(&reporter), vec![0.400, 0.400]);
        assert_eq!(tracker.last_progress(), 0.400);
    }

    #[tokio::test]
    async fn unknown_alive_does_not_advance_progress() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.400, "Converting").await;
        tracker.unknown_alive("Still running").await;
        assert_eq!(progress_values(&reporter), vec![0.400, 0.400]);
        assert_eq!(tracker.last_confidence(), ProgressConfidence::Unknown);
    }

    #[tokio::test]
    async fn failure_bypasses_throttle_and_preserves_progress() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.250, "Converting").await;
        tracker.failure("Encoder failed").await;
        assert_eq!(progress_values(&reporter), vec![0.250, 0.250]);
        assert_eq!(
            progress_messages(&reporter),
            vec!["Converting", "Encoder failed"]
        );
    }

    #[tokio::test]
    async fn cancel_requested_bypasses_throttle() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.estimated(0.250, "Converting").await;
        tracker.cancel_requested().await;
        let messages = progress_messages(&reporter);
        assert_eq!(messages, vec!["Converting", "Cancelling…"]);
    }

    #[tokio::test]
    async fn elapsed_text_appears_after_threshold() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter))
                .with_elapsed_threshold(Duration::from_secs(0));
        tracker.estimated(0.100, "Converting").await;
        let messages = progress_messages(&reporter);
        assert!(messages[0].starts_with("Converting · elapsed "));
    }

    #[tokio::test]
    async fn eta_hidden_during_first_unit_without_tool_progress() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.start_unit(1, 3, "Track 1").await;
        tracker.estimated(0.100, "Converting track 1").await;
        assert!(progress_messages(&reporter)
            .iter()
            .all(|message| !message.contains("remaining")));
    }

    #[tokio::test]
    async fn eta_appears_after_first_unit_completes() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.start_unit(1, 3, "Track 1").await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.finish_unit(1, 3, "Track 1").await;
        let messages = progress_messages(&reporter);
        assert!(messages
            .last()
            .expect("message")
            .contains("about 2m remaining"));
    }

    #[tokio::test]
    async fn eta_uses_sample_weight_when_available() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker
            .start_weighted_unit(1, 3, "Short", 1_000, 6_000)
            .await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker
            .finish_weighted_unit(1, 3, "Short", 1_000, 6_000)
            .await;
        let messages = progress_messages(&reporter);
        assert!(messages
            .last()
            .expect("message")
            .contains("about 5m remaining"));
    }

    #[tokio::test]
    async fn eta_hidden_after_failure() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.start_unit(1, 3, "Track 1").await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.finish_unit(1, 3, "Track 1").await;
        tracker.failure("Failed").await;
        tracker
            .estimated_with_key(0.50, "after-fail", "After fail")
            .await;
        assert!(!progress_messages(&reporter)
            .last()
            .expect("message")
            .contains("remaining"));
    }

    #[tokio::test]
    async fn eta_hidden_after_cancellation() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.start_unit(1, 3, "Track 1").await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.finish_unit(1, 3, "Track 1").await;
        tracker.cancel_requested().await;
        tracker
            .estimated_with_key(0.50, "after-cancel", "After cancel")
            .await;
        assert!(!progress_messages(&reporter)
            .last()
            .expect("message")
            .contains("remaining"));
    }

    #[tokio::test]
    async fn eta_hidden_after_denominator_change() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.start_unit(1, 3, "Track 1").await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.finish_unit(1, 3, "Track 1").await;
        tracker.start_unit(2, 2, "Track 2").await;
        tracker
            .estimated_with_key(0.75, "after-skip", "After skip")
            .await;
        assert!(!progress_messages(&reporter)
            .last()
            .expect("message")
            .contains("remaining"));
    }

    #[tokio::test]
    async fn eta_not_shown_for_unknown_confidence() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.start_unit(1, 2, "Track 1").await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.finish_unit(1, 2, "Track 1").await;
        tracker
            .unknown_alive_with_key("heartbeat", "Still running")
            .await;
        assert!(!progress_messages(&reporter)
            .last()
            .expect("message")
            .contains("remaining"));
    }

    #[tokio::test]
    async fn non_emitting_weighted_hooks_feed_eta_into_existing_messages() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.observe_weighted_unit_start(1, 3, Some(1_000), Some(6_000));
        tracker
            .estimated(0.10, "Converting track 1 of 3: Short")
            .await;
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.observe_weighted_unit_finish(1, 3, Some(1_000), Some(6_000));
        tracker
            .estimated_with_key(0.20, "finished-1", "Finished track 1 of 3: Short")
            .await;

        let messages = progress_messages(&reporter);
        assert_eq!(messages[0], "Converting track 1 of 3: Short");
        assert!(messages
            .last()
            .expect("message")
            .contains("about 5m remaining"));
    }

    #[tokio::test]
    async fn suppress_eta_hides_eta_on_failure_style_estimated_updates() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.observe_unit_start(1, 3);
        tracker.force_active_eta_unit_elapsed_for_tests(Duration::from_secs(60));
        tracker.observe_unit_finish(1, 3);
        tracker
            .estimated_with_key(0.33, "finished-1", "Finished track 1")
            .await;
        tracker.suppress_eta();
        tracker
            .estimated_with_key(0.66, "failed-2", "Track 2 of 3 failed: encoder failed")
            .await;
        tracker
            .estimated_with_key(0.90, "after-failure", "Converting track 3 of 3")
            .await;

        let messages = progress_messages(&reporter);
        assert!(messages[0].contains("remaining"));
        assert!(!messages[1].contains("remaining"));
        assert!(!messages[2].contains("remaining"));
    }

    #[tokio::test]
    async fn tool_cancel_message_names_tool() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.cancel_requested_for_tool("ffmpeg").await;
        assert_eq!(progress_messages(&reporter), vec!["Stopping ffmpeg…"]);
    }

    #[tokio::test]
    async fn cancelled_at_message_preserves_progress_point() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.measured_with_key(0.37, "ffmpeg", "Encoding").await;
        tracker.cancelled_at_last_progress().await;
        assert_eq!(
            progress_messages(&reporter),
            vec!["Encoding", "Cancelled at 37%"]
        );
    }

    #[tokio::test]
    async fn cancelled_at_locks_out_later_progress_messages() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        tracker.measured_with_key(0.37, "ffmpeg", "Encoding").await;
        tracker.cancelled_at_last_progress().await;
        tracker
            .estimated_with_key(0.80, "late-failure", "Track 2 of 3 failed: cancelled")
            .await;
        tracker.cancel_requested().await;

        assert_eq!(
            progress_messages(&reporter),
            vec!["Encoding", "Cancelled at 37%"]
        );
    }
}
