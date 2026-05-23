//! Error types returned by pure planning.

use crate::enums::{AudioFormat, PreferredTool};
use crate::tools::ToolIdentifier;
use std::error::Error;
use std::fmt;

/// Convenient result alias for pipeline planning.
pub type Result<T> = std::result::Result<T, PlanningError>;

/// Planning failures detected before any process execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanningError {
    /// A settings field has an invalid value or impossible combination.
    InvalidSettings {
        /// Field or group name.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// Source facts are missing or inconsistent.
    InvalidSource {
        /// Field or group name.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// No registered plugin can build the requested operation.
    NoPluginForOperation {
        /// Logical operation label.
        operation: String,
        /// Preferred tool at the time selection failed.
        preferred_tool: PreferredTool,
    },
    /// A registered plugin claimed support but rejected the final build request.
    PluginRejectedOperation {
        /// Plugin identity.
        tool: ToolIdentifier,
        /// Human-readable reason.
        reason: String,
    },
    /// The target format is not supported for this path.
    UnsupportedFormat {
        /// Target format.
        format: AudioFormat,
        /// Human-readable reason.
        reason: String,
    },
    /// A caller attempted to register an invalid or duplicate plugin.
    RegistryError {
        /// Human-readable reason.
        reason: String,
    },
}

impl PlanningError {
    /// Construct an invalid-settings error.
    #[must_use]
    pub fn invalid_settings(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidSettings {
            field,
            reason: reason.into(),
        }
    }

    /// Construct an invalid-source error.
    #[must_use]
    pub fn invalid_source(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidSource {
            field,
            reason: reason.into(),
        }
    }

    /// Construct an unsupported-format error.
    #[must_use]
    pub fn unsupported_format(format: AudioFormat, reason: impl Into<String>) -> Self {
        Self::UnsupportedFormat {
            format,
            reason: reason.into(),
        }
    }

    /// Construct a plugin rejection error.
    #[must_use]
    pub fn plugin_rejected(tool: ToolIdentifier, reason: impl Into<String>) -> Self {
        Self::PluginRejectedOperation {
            tool,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for PlanningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSettings { field, reason } => {
                write!(f, "invalid settings for {field}: {reason}")
            }
            Self::InvalidSource { field, reason } => {
                write!(f, "invalid source facts for {field}: {reason}")
            }
            Self::NoPluginForOperation {
                operation,
                preferred_tool,
            } => write!(
                f,
                "no registered plugin can build operation {operation} with preference {preferred_tool:?}"
            ),
            Self::PluginRejectedOperation { tool, reason } => {
                write!(f, "plugin {tool} rejected operation: {reason}")
            }
            Self::UnsupportedFormat { format, reason } => {
                write!(f, "unsupported format {format}: {reason}")
            }
            Self::RegistryError { reason } => write!(f, "plugin registry error: {reason}"),
        }
    }
}

impl Error for PlanningError {}
