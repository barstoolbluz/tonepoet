//! PR 1 — every error type the pipeline contracts use.
//!
//! Error contracts shared across the conversion pipeline.

use std::fmt;
use std::time::Duration;

use thiserror::Error;

use super::tool::{CommandRecord, ProcessExit};
use super::types::{BlockedSource, SourceKind};

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
    #[error("{message}")]
    BlockedSource {
        message: String,
        blocked: Box<BlockedSource>,
    },
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

#[derive(Debug)]
pub enum ToolRunnerError {
    Spawn { command: CommandRecord },
    Timeout {
        elapsed: Duration,
        command: CommandRecord,
    },
    Cancelled { command: CommandRecord },
    UnsupportedPipeline,
    Termination {
        message: String,
        command: CommandRecord,
    },
    NonZeroExit {
        exit: ProcessExit,
        stderr_tail: String,
        command: CommandRecord,
    },
    Io(std::io::Error),
}

const TOOL_ERROR_STDERR_DISPLAY_CHARS: usize = 512;

fn compact_tool_stderr(stderr: &str) -> Option<String> {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut chars = trimmed.chars().rev();
    let mut tail = chars
        .by_ref()
        .take(TOOL_ERROR_STDERR_DISPLAY_CHARS)
        .collect::<Vec<_>>();
    let truncated = chars.next().is_some();
    tail.reverse();
    let compact = tail
        .into_iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(if truncated {
        format!("... {compact}")
    } else {
        compact
    })
}

fn process_exit_text(exit: ProcessExit) -> String {
    match exit {
        ProcessExit::Code(code) => format!("exit code {code}"),
        ProcessExit::Signal(signal) => format!("signal {signal}"),
        ProcessExit::Unknown => "unknown exit status".to_string(),
    }
}

impl fmt::Display for ToolRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { .. } => f.write_str("failed to spawn tool"),
            Self::Timeout { elapsed, .. } => write!(f, "tool timed out after {elapsed:?}"),
            Self::Cancelled { .. } => f.write_str("tool cancelled"),
            Self::UnsupportedPipeline => {
                f.write_str("tool runner does not support typed pipelines")
            }
            Self::Termination { message, .. } => {
                write!(f, "tool termination/reaping failed: {message}")
            }
            Self::NonZeroExit {
                exit,
                stderr_tail,
                command,
            } => {
                write!(
                    f,
                    "{}: tool exited non-zero ({})",
                    command.binary.canonical_name(),
                    process_exit_text(*exit),
                )?;
                if let Some(stderr) = compact_tool_stderr(stderr_tail) {
                    write!(f, "; stderr: {stderr}")
                } else {
                    f.write_str("; no stderr output")
                }
            }
            Self::Io(error) => write!(f, "io error running tool: {error}"),
        }
    }
}

impl std::error::Error for ToolRunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ToolRunnerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
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
    #[error("{0}")]
    PolicyRejected(&'static str),
    #[error("Reference metadata toolchain identity failed: {0}")]
    ReferenceToolchain(String),
    #[error("unsupported tag format: {0}")]
    UnsupportedTagFormat(String),
    #[error("in-process metadata write failed: {0}")]
    InProcessWrite(String),
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
    #[error("manifest authority failed: {0}")]
    Manifest(String),
    #[error("chapter structure finalization failed: {0}")]
    ChapterStructure(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// An IO failure with the operation and path attached. Publish runs many
    /// small filesystem steps across staging, temp, and album directories; a
    /// bare "No such file or directory" is undiagnosable without knowing which
    /// step and which path failed.
    #[error("{op} {path}: {source}")]
    IoAt {
        op: &'static str,
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl PublishError {
    /// Build a `map_err` closure that attaches the failing operation and path.
    pub fn io_at(op: &'static str, path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::IoAt { op, path, source }
    }
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
