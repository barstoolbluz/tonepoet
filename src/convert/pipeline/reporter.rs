//! PR 1 — pipeline event reporting contract.
//!
//! `run_pipeline_item` emits `PipelineEvent`s through a
//! `PipelineReporter`. Tests subscribe to a `RecordingReporter` to
//! prove terminal-event ordering directly.

use std::sync::Mutex;

use async_trait::async_trait;

use super::types::{PipelineStage, StageRecord};
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
