//! Route-neutral progress instrumentation for pipeline stages.
//!
//! This module sits between long-running stage code and `PipelineReporter`.
//! It emits the existing `PipelineEvent::Progress` variant; it does not alter
//! the public reporter, event, progress-update, or conversion-status contracts.
//!
//! Unit ordinals and totals are intentionally kept internal here. The locked
//! `PipelineEvent::Progress` contract has no structured unit-progress field, so
//! Milestone 1 reports unit identity in the progress message. Populating
//! `ConversionStatus::Processing::file_progress` structurally requires a later
//! reporter/event contract extension or an agreed side-channel.

pub mod confidence;
pub mod elapsed;
pub mod operation;
pub mod throttle;

pub use confidence::{ProgressConfidence, ProgressScope};
pub use elapsed::{append_elapsed, format_elapsed, DEFAULT_ELAPSED_THRESHOLD};
pub use operation::OperationProgressTracker;
pub use throttle::{ProgressThrottle, DEFAULT_MIN_INTERVAL, DEFAULT_MIN_PROGRESS_DELTA};
