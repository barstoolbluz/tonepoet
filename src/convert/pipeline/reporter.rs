//! PR 1 — pipeline event reporting contract.
//!
//! `run_pipeline_item` emits `PipelineEvent`s through a
//! `PipelineReporter`. Tests subscribe to a `RecordingReporter` to
//! prove terminal-event ordering directly.

use std::sync::Mutex;

use async_trait::async_trait;

use super::types::{PipelineStage, StageOutcome, StageRecord};
use crate::convert::ConversionStatus;

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    StageStarted {
        item_id: String,
        stage: PipelineStage,
    },
    StageFinished {
        item_id: String,
        record: StageRecord,
    },
    Progress {
        item_id: String,
        stage: PipelineStage,
        phase_progress: f32,
        message: Option<String>,
    },
    Terminal {
        item_id: String,
        status: ConversionStatus,
    },
}

#[async_trait]
pub trait PipelineReporter: Send + Sync {
    async fn emit(&self, event: PipelineEvent);
}

/// Stores every emitted event for ordering assertions in tests.
pub struct RecordingReporter {
    events: Mutex<Vec<PipelineEvent>>,
}

impl RecordingReporter {
    pub fn new() -> Self {
        Self { events: Mutex::new(Vec::new()) }
    }

    /// All emitted events, in emission order.
    pub fn events(&self) -> Vec<PipelineEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl Default for RecordingReporter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineReporter for RecordingReporter {
    async fn emit(&self, event: PipelineEvent) {
        self.events.lock().unwrap().push(event);
    }
}

// ===========================================================================
// TUI broadcast reporter
// ===========================================================================

#[derive(Debug, Clone, Copy)]
struct StageProgressWindow {
    phase: crate::convert::ConversionPhase,
    start: f32,
    end: f32,
}

#[derive(Debug)]
struct BroadcastReporterState {
    last_progress: f32,
    last_phase: Option<crate::convert::ConversionPhase>,
    last_message: Option<String>,
    last_emit_at: Option<std::time::Instant>,
}

impl Default for BroadcastReporterState {
    fn default() -> Self {
        Self {
            last_progress: 0.0,
            last_phase: None,
            last_message: None,
            last_emit_at: None,
        }
    }
}

/// Forwards pipeline events to the TUI progress broadcast channel.
///
/// `RecordingReporter` remains available for tests that need the full event
/// stream. This reporter is for live UI updates: it maps each pipeline stage to
/// the existing `ConversionStatus::Processing` shape, keeps visible progress
/// monotonic, and preserves the last observed progress for failed or cancelled
/// terminal states.
pub struct BroadcastReporter {
    tx: tokio::sync::broadcast::Sender<crate::convert::ProgressUpdate>,
    item_id: String,
    state: std::sync::Mutex<BroadcastReporterState>,
}

impl BroadcastReporter {
    pub fn new(
        tx: tokio::sync::broadcast::Sender<crate::convert::ProgressUpdate>,
        item_id: impl Into<String>,
    ) -> Self {
        Self {
            tx,
            item_id: item_id.into(),
            state: std::sync::Mutex::new(BroadcastReporterState::default()),
        }
    }

    fn window(stage: PipelineStage) -> StageProgressWindow {
        match stage {
            PipelineStage::Materialize => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Extracting,
                start: 0.0,
                end: 15.0,
            },
            PipelineStage::PlanOutputs => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Analyzing,
                start: 15.0,
                end: 20.0,
            },
            PipelineStage::Convert => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Converting,
                start: 20.0,
                end: 80.0,
            },
            PipelineStage::Merge => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Converting,
                start: 80.0,
                end: 85.0,
            },
            PipelineStage::Metadata => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Tagging,
                start: 85.0,
                end: 90.0,
            },
            PipelineStage::ReplayGain => StageProgressWindow {
                phase: crate::convert::ConversionPhase::PostProcessing,
                start: 90.0,
                end: 93.0,
            },
            PipelineStage::Features => StageProgressWindow {
                phase: crate::convert::ConversionPhase::PostProcessing,
                start: 93.0,
                end: 95.0,
            },
            PipelineStage::Publish => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Finalizing,
                start: 95.0,
                end: 98.0,
            },
            PipelineStage::DurableLog => StageProgressWindow {
                phase: crate::convert::ConversionPhase::Finalizing,
                start: 98.0,
                end: 100.0,
            },
        }
    }

    fn stage_label(stage: PipelineStage) -> &'static str {
        match stage {
            PipelineStage::Materialize => "Extracting source",
            PipelineStage::PlanOutputs => "Analyzing outputs",
            PipelineStage::Convert => "Converting audio",
            PipelineStage::Merge => "Merging tracks",
            PipelineStage::Metadata => "Writing metadata",
            PipelineStage::ReplayGain => "Calculating ReplayGain",
            PipelineStage::Features => "Writing sidecars",
            PipelineStage::Publish => "Publishing files",
            PipelineStage::DurableLog => "Writing conversion log",
        }
    }

    fn stage_complete_message(stage: PipelineStage) -> String {
        match stage {
            PipelineStage::Materialize => "Source extracted".to_string(),
            PipelineStage::PlanOutputs => "Output plan ready".to_string(),
            PipelineStage::Convert => "Audio conversion complete".to_string(),
            PipelineStage::Merge => "Track merge complete".to_string(),
            PipelineStage::Metadata => "Metadata written".to_string(),
            PipelineStage::ReplayGain => "ReplayGain complete".to_string(),
            PipelineStage::Features => "Sidecars written".to_string(),
            PipelineStage::Publish => "Files published".to_string(),
            PipelineStage::DurableLog => "Conversion log written".to_string(),
        }
    }

    fn stage_skipped_message(stage: PipelineStage) -> String {
        match stage {
            PipelineStage::Materialize => "Source extraction skipped".to_string(),
            PipelineStage::PlanOutputs => "Output planning skipped".to_string(),
            PipelineStage::Convert => "Audio conversion skipped".to_string(),
            PipelineStage::Merge => "Track merge skipped".to_string(),
            PipelineStage::Metadata => "Metadata skipped".to_string(),
            PipelineStage::ReplayGain => "ReplayGain skipped".to_string(),
            PipelineStage::Features => "Feature extraction skipped".to_string(),
            PipelineStage::Publish => "Publishing skipped".to_string(),
            PipelineStage::DurableLog => "Conversion log skipped".to_string(),
        }
    }

    fn round_progress(progress: f32) -> f32 {
        ((progress.clamp(0.0, 100.0) * 10.0).round()) / 10.0
    }

    fn phase_progress(window: StageProgressWindow, progress: f32) -> f32 {
        if window.end <= window.start {
            return 100.0;
        }
        let phase_progress = ((progress - window.start) / (window.end - window.start)) * 100.0;
        Self::round_progress(phase_progress)
    }

    fn should_send(
        state: &BroadcastReporterState,
        phase: crate::convert::ConversionPhase,
        progress: f32,
        message: &Option<String>,
        force: bool,
        now: std::time::Instant,
    ) -> bool {
        if force {
            return true;
        }
        if state.last_phase != Some(phase) {
            return true;
        }
        if state.last_message.as_ref() != message.as_ref() {
            return true;
        }
        if progress - state.last_progress >= 0.5 {
            return true;
        }
        state
            .last_emit_at
            .map(|last| now.duration_since(last) >= std::time::Duration::from_millis(500))
            .unwrap_or(true)
    }

    fn send_processing(
        &self,
        stage: PipelineStage,
        requested_progress: f32,
        message: Option<String>,
        force: bool,
    ) {
        let window = Self::window(stage);
        let now = std::time::Instant::now();
        let progress;
        {
            let mut state = self.state.lock().unwrap();
            let monotonic = requested_progress.max(state.last_progress);
            let rounded = Self::round_progress(monotonic);
            if !Self::should_send(&state, window.phase, rounded, &message, force, now) {
                return;
            }
            state.last_progress = rounded;
            state.last_phase = Some(window.phase);
            state.last_message = message.clone();
            state.last_emit_at = Some(now);
            progress = rounded;
        }

        let _ = self.tx.send(crate::convert::ProgressUpdate {
            item_id: self.item_id.clone(),
            progress,
            status: crate::convert::ConversionStatus::Processing {
                progress,
                message,
                file_progress: None,
                phase: Some(window.phase),
                phase_progress: Some(Self::phase_progress(window, progress)),
            },
        });
    }

    fn send_terminal(&self, status: crate::convert::ConversionStatus) {
        let progress = {
            let mut state = self.state.lock().unwrap();
            let progress = match &status {
                crate::convert::ConversionStatus::Processing { progress, .. } => {
                    Self::round_progress((*progress).max(state.last_progress))
                }
                crate::convert::ConversionStatus::Completed { .. }
                | crate::convert::ConversionStatus::Partial { .. } => 100.0,
                crate::convert::ConversionStatus::Failed { .. }
                | crate::convert::ConversionStatus::Cancelled => state.last_progress,
                crate::convert::ConversionStatus::Queued
                | crate::convert::ConversionStatus::Paused
                | crate::convert::ConversionStatus::NotConfigured => 0.0,
            };
            state.last_progress = progress;
            state.last_emit_at = Some(std::time::Instant::now());
            progress
        };

        let _ = self.tx.send(crate::convert::ProgressUpdate {
            item_id: self.item_id.clone(),
            progress,
            status,
        });
    }
}

#[async_trait::async_trait]
impl PipelineReporter for BroadcastReporter {
    async fn emit(&self, event: PipelineEvent) {
        match event {
            PipelineEvent::StageStarted { item_id, stage } => {
                if item_id != self.item_id {
                    return;
                }
                let window = Self::window(stage);
                self.send_processing(
                    stage,
                    window.start,
                    Some(Self::stage_label(stage).to_string()),
                    true,
                );
            }
            PipelineEvent::StageFinished { item_id, record } => {
                if item_id != self.item_id {
                    return;
                }
                match record.outcome {
                    StageOutcome::Ok => {
                        let window = Self::window(record.stage);
                        self.send_processing(
                            record.stage,
                            window.end,
                            Some(Self::stage_complete_message(record.stage)),
                            true,
                        );
                    }
                    StageOutcome::Skipped => {
                        let window = Self::window(record.stage);
                        self.send_processing(
                            record.stage,
                            window.end,
                            Some(Self::stage_skipped_message(record.stage)),
                            true,
                        );
                    }
                    StageOutcome::Failed(error) => {
                        let current = self.state.lock().unwrap().last_progress;
                        self.send_processing(
                            record.stage,
                            current,
                            Some(format!(
                                "{} failed: {}",
                                Self::stage_label(record.stage),
                                error
                            )),
                            true,
                        );
                    }
                }
            }
            PipelineEvent::Progress {
                item_id,
                stage,
                phase_progress,
                message,
            } => {
                if item_id != self.item_id {
                    return;
                }
                let window = Self::window(stage);
                let unit_progress = phase_progress.clamp(0.0, 1.0);
                let progress = window.start + ((window.end - window.start) * unit_progress);
                self.send_processing(stage, progress, message, false);
            }
            PipelineEvent::Terminal { item_id, status } => {
                if item_id != self.item_id {
                    return;
                }
                self.send_terminal(status);
            }
        }
    }
}

#[cfg(test)]
mod broadcast_reporter_tests {
    use super::*;
    use std::path::PathBuf;

    fn reporter_pair() -> (
        BroadcastReporter,
        tokio::sync::broadcast::Receiver<crate::convert::ProgressUpdate>,
    ) {
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        (BroadcastReporter::new(tx, "item-1"), rx)
    }

    async fn next_update(
        rx: &mut tokio::sync::broadcast::Receiver<crate::convert::ProgressUpdate>,
    ) -> crate::convert::ProgressUpdate {
        rx.recv().await.expect("progress update")
    }

    #[tokio::test]
    async fn stage_start_maps_to_phase_and_window_start() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::StageStarted {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Convert,
            })
            .await;

        let update = next_update(&mut rx).await;
        assert_eq!(update.progress, 20.0);
        match update.status {
            crate::convert::ConversionStatus::Processing {
                phase,
                phase_progress,
                message,
                ..
            } => {
                assert_eq!(phase, Some(crate::convert::ConversionPhase::Converting));
                assert_eq!(phase_progress, Some(0.0));
                assert_eq!(message.as_deref(), Some("Converting audio"));
            }
            other => panic!("expected processing update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn convert_progress_maps_into_convert_window() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::Progress {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Convert,
                phase_progress: 0.5,
                message: Some("Converting track 1 of 2".to_string()),
            })
            .await;

        let update = next_update(&mut rx).await;
        assert_eq!(update.progress, 50.0);
        match update.status {
            crate::convert::ConversionStatus::Processing {
                phase_progress, ..
            } => assert_eq!(phase_progress, Some(50.0)),
            other => panic!("expected processing update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn processing_progress_is_monotonic() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::Progress {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Convert,
                phase_progress: 1.0,
                message: Some("Converted track 2 of 2".to_string()),
            })
            .await;
        let first = next_update(&mut rx).await;
        assert_eq!(first.progress, 80.0);

        reporter
            .emit(PipelineEvent::Progress {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Convert,
                phase_progress: 0.25,
                message: Some("late duplicate progress".to_string()),
            })
            .await;
        let second = next_update(&mut rx).await;
        assert_eq!(second.progress, 80.0);
    }

    #[tokio::test]
    async fn failed_terminal_preserves_last_progress() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::Progress {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Convert,
                phase_progress: 0.25,
                message: Some("Converting track 1 of 4".to_string()),
            })
            .await;
        let progress = next_update(&mut rx).await;
        assert_eq!(progress.progress, 35.0);

        reporter
            .emit(PipelineEvent::Terminal {
                item_id: "item-1".to_string(),
                status: crate::convert::ConversionStatus::Failed {
                    error: "encoder failed".to_string(),
                    log_path: None,
                },
            })
            .await;
        let terminal = next_update(&mut rx).await;
        assert_eq!(terminal.progress, 35.0);
    }

    #[tokio::test]
    async fn completed_terminal_reaches_one_hundred() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::Terminal {
                item_id: "item-1".to_string(),
                status: crate::convert::ConversionStatus::Completed {
                    output_path: PathBuf::from("/tmp/out.flac"),
                    log_path: None,
                },
            })
            .await;

        let terminal = next_update(&mut rx).await;
        assert_eq!(terminal.progress, 100.0);
    }


    #[tokio::test]
    async fn stage_finish_maps_to_window_end() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::StageFinished {
                item_id: "item-1".to_string(),
                record: StageRecord {
                    stage: PipelineStage::ReplayGain,
                    outcome: StageOutcome::Ok,
                },
            })
            .await;

        let update = next_update(&mut rx).await;
        assert_eq!(update.progress, 93.0);
        match update.status {
            crate::convert::ConversionStatus::Processing { message, phase, .. } => {
                assert_eq!(phase, Some(crate::convert::ConversionPhase::PostProcessing));
                assert_eq!(message.as_deref(), Some("ReplayGain complete"));
            }
            other => panic!("expected processing update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn skipped_stage_reports_skip_message_at_window_end() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::StageFinished {
                item_id: "item-1".to_string(),
                record: StageRecord {
                    stage: PipelineStage::Features,
                    outcome: StageOutcome::Skipped,
                },
            })
            .await;

        let update = next_update(&mut rx).await;
        assert_eq!(update.progress, 95.0);
        match update.status {
            crate::convert::ConversionStatus::Processing { message, .. } => {
                assert_eq!(message.as_deref(), Some("Feature extraction skipped"));
            }
            other => panic!("expected processing update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancelled_terminal_preserves_last_progress() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::Progress {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Convert,
                phase_progress: 0.5,
                message: Some("Converting track 1 of 2".to_string()),
            })
            .await;
        let progress = next_update(&mut rx).await;
        assert_eq!(progress.progress, 50.0);

        reporter
            .emit(PipelineEvent::Terminal {
                item_id: "item-1".to_string(),
                status: crate::convert::ConversionStatus::Cancelled,
            })
            .await;
        let terminal = next_update(&mut rx).await;
        assert_eq!(terminal.progress, 50.0);
    }

    #[tokio::test]
    async fn partial_terminal_reaches_one_hundred() {
        let (reporter, mut rx) = reporter_pair();
        reporter
            .emit(PipelineEvent::Terminal {
                item_id: "item-1".to_string(),
                status: crate::convert::ConversionStatus::Partial {
                    output_path: PathBuf::from("/tmp/out"),
                    successful: 8,
                    failed: 1,
                    log_path: PathBuf::from("/tmp/log.json"),
                },
            })
            .await;

        let terminal = next_update(&mut rx).await;
        assert_eq!(terminal.progress, 100.0);
    }

    #[tokio::test]
    async fn send_error_does_not_fail_emit() {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(rx);
        let reporter = BroadcastReporter::new(tx, "item-1");
        reporter
            .emit(PipelineEvent::StageStarted {
                item_id: "item-1".to_string(),
                stage: PipelineStage::Materialize,
            })
            .await;
    }
}

