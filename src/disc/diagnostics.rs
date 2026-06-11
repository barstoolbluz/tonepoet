use super::model::PresentationId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticScope {
    Disc,
    Presentation(PresentationId),
    Track,
    SuppressedCandidate,
}

/// A normalized, user-facing diagnostic from disc parsing or probing.
#[derive(Debug, Clone)]
pub struct DiscDiagnostic {
    pub severity: DiagnosticSeverity,
    pub scope: DiagnosticScope,
    pub message: String,
}
