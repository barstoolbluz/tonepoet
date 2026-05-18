//! Typed progress metadata for route-neutral instrumentation.

/// Describes how trustworthy a progress value is.
///
/// These values are internal to the progress subsystem. They do not alter the
/// existing `PipelineEvent` contract; they let operation helpers keep measured,
/// estimated, and unknown progress separate without parsing message strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressConfidence {
    /// Progress came from a reliable source, such as tool output or a byte
    /// counter with a known denominator.
    Measured,
    /// Progress came from a known workload model, such as samples, duration, or
    /// item count.
    Estimated,
    /// Work is active, but no meaningful denominator is available.
    Unknown,
}

/// The logical scope of a progress update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressScope {
    Overall,
    Stage,
    File,
    Track,
    Tool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_is_typed() {
        let confidence = ProgressConfidence::Measured;
        assert!(matches!(confidence, ProgressConfidence::Measured));
        assert_ne!(confidence, ProgressConfidence::Estimated);
    }

    #[test]
    fn scope_is_typed() {
        let scope = ProgressScope::Track;
        assert!(matches!(scope, ProgressScope::Track));
        assert_ne!(scope, ProgressScope::Tool);
    }
}
