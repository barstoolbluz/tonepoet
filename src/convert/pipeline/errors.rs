//! PR 1 — every error type the pipeline contracts use.
//!
//! All `thiserror`-derived. No later PR adds a new error type.

use std::time::Duration;

use thiserror::Error;

use super::tool::{CommandRecord, ProcessExit};
use super::types::SourceKind;

#[derive(Debug, Error)]
pub enum RequestValidationError {
    #[error("request has no container path")]
    MissingContainer,
    #[error("invalid output root: {0}")]
    InvalidOutputRoot(String),
    #[error("invalid naming template: {0}")]
    InvalidTemplate(String),
    #[error("invalid secret state: {0}")]
    InvalidSecretState(String),
    #[error("invalid stage policy: {0}")]
    InvalidStagePolicy(String),
}

#[derive(Debug, Error)]
pub enum SourceDetectError {
    #[error("unrecognized source container")]
    UnknownSource,
    #[error("ambiguous CUE layout: {0}")]
    AmbiguousCue(String),
    #[error("io error during source detection: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum SourceDispatchError {
    #[error("no materializer for source kind {0:?}")]
    Unsupported(SourceKind),
}

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("archive/container extraction failed: {0}")]
    Extraction(String),
    #[error("source parse failed: {0}")]
    Parse(String),
    #[error("source is encrypted")]
    Encrypted,
    #[error("invalid track selection: {0}")]
    InvalidTrackSelection(String),
    #[error("materialization cancelled")]
    Cancelled,
    #[error("tool error: {0}")]
    Tool(#[from] ToolRunnerError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("output naming collision: {0}")]
    NamingCollision(String),
    #[error("invalid naming template: {0}")]
    InvalidTemplate(String),
    #[error("manifest has no tracks")]
    EmptyManifest,
    #[error("invalid track selection: {0}")]
    InvalidTrackSelection(String),
    #[error("planned path escapes the output root: {0}")]
    PathOutsideOutputRoot(String),
}

#[derive(Debug, Error)]
pub enum ToolRunnerError {
    #[error("failed to spawn tool")]
    Spawn { command: CommandRecord },
    #[error("tool timed out after {elapsed:?}")]
    Timeout {
        elapsed: Duration,
        command: CommandRecord,
    },
    #[error("tool cancelled")]
    Cancelled { command: CommandRecord },
    #[error("tool exited non-zero")]
    NonZeroExit {
        exit: ProcessExit,
        stderr_tail: String,
        command: CommandRecord,
    },
    #[error("io error running tool: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("track source kind not yet supported")]
    UnsupportedTrackSource,
    #[error("track realization failed: {0}")]
    Realize(String),
    #[error("track validation failed: {0}")]
    TrackValidation(String),
    #[error("backend encode failed: {0}")]
    Backend(String),
    #[error("tool error: {0}")]
    Tool(#[from] ToolRunnerError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("merged duration/sample-count mismatch: {0}")]
    DurationMismatch(String),
    #[error("merge unsupported for this format: {0}")]
    UnsupportedFormat(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tool error: {0}")]
    Tool(#[from] ToolRunnerError),
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("unsupported tag format: {0}")]
    UnsupportedTagFormat(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tool error: {0}")]
    Tool(#[from] ToolRunnerError),
}

#[derive(Debug, Error)]
pub enum ReplayGainError {
    #[error("unsupported format for ReplayGain: {0}")]
    UnsupportedFormat(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tool error: {0}")]
    Tool(#[from] ToolRunnerError),
}

#[derive(Debug, Error)]
pub enum FeatureError {
    #[error("CUE sheet generation failed: {0}")]
    CueGeneration(String),
    #[error("conversion log generation failed: {0}")]
    ConversionLogGeneration(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("staging directory missing")]
    StagingMissing,
    #[error("destination already exists: {0}")]
    DestinationExists(String),
    #[error("planned path escapes the output root: {0}")]
    PathOutsideOutputRoot(String),
    #[error("cross-device copy failed: {0}")]
    CrossDeviceCopy(String),
    #[error("atomic rename failed: {0}")]
    AtomicRename(String),
    #[error("backup of existing destination failed: {0}")]
    BackupFailed(String),
    #[error("rollback after failed publish failed: {0}")]
    RollbackFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum LogError {
    #[error("io error writing durable log: {0}")]
    Io(#[from] std::io::Error),
    #[error("durable log serialization failed: {0}")]
    Serialization(String),
}

/// Single error type for orchestration callers that need one.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    RequestValidation(#[from] RequestValidationError),
    #[error(transparent)]
    SourceDetect(#[from] SourceDetectError),
    #[error(transparent)]
    SourceDispatch(#[from] SourceDispatchError),
    #[error(transparent)]
    Materialize(#[from] MaterializeError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Convert(#[from] ConvertError),
    #[error(transparent)]
    Merge(#[from] MergeError),
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    #[error(transparent)]
    ReplayGain(#[from] ReplayGainError),
    #[error(transparent)]
    Feature(#[from] FeatureError),
    #[error(transparent)]
    Publish(#[from] PublishError),
    #[error(transparent)]
    Log(#[from] LogError),
}
