#![forbid(unsafe_code)]

use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DvdaError>;

#[derive(Debug, Error)]
pub enum DvdaError {
    #[error("I/O error while accessing {path}: {source}")]
    Io { path: String, #[source] source: io::Error },

    #[error("missing DVD-Audio file; tried {candidates:?}")]
    MissingFile { candidates: Vec<String> },

    #[error("{file} has invalid identifier: expected {expected}, got {got:?}")]
    InvalidIdentifier { file: String, expected: &'static str, got: String },

    #[error("short {context}: need at least {needed} bytes, have {available}")]
    ShortRead { context: String, needed: usize, available: usize },

    #[error("{context} references bytes outside buffer: offset={offset}, len={len}, available={available}")]
    OutOfBounds { context: String, offset: usize, len: usize, available: usize },

    #[error("parse error in {context}: {message}")]
    Parse { context: String, message: String },

    #[error("unsupported DVD-Audio feature in Phase 1: {feature}")]
    Unsupported { feature: String },

    #[error("ISO/UDF backend error: {message}")]
    Iso { message: String },
}

impl DvdaError {
    pub fn io(path: impl Into<String>, source: io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    pub fn parse(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Parse { context: context.into(), message: message.into() }
    }

    pub fn bounds(context: impl Into<String>, offset: usize, len: usize, available: usize) -> Self {
        Self::OutOfBounds { context: context.into(), offset, len, available }
    }
}
