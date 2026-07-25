//! Shared type-to-select matching policy for flat file and tree panes.

use std::time::{Duration, Instant};

/// Shared inactivity timeout for incremental type-to-select buffers.
pub const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(1500);

/// A lightweight item description consumed by the pure matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeAheadCandidate<'a> {
    pub name: &'a str,
    pub is_dir: bool,
}

/// Return the first UX-compatible match for `query`.
///
/// Matching is case-insensitive, directory-first, and prefix-first within each
/// kind. Substring matches are considered only when no prefix matches exist.
/// The scan always starts at index zero and never wraps or cycles.
pub fn first_type_ahead_match<'a, I>(candidates: I, query: &str) -> Option<usize>
where
    I: IntoIterator<Item = TypeAheadCandidate<'a>>,
{
    if query.is_empty() {
        return None;
    }
    let query = query.to_lowercase();
    let candidates: Vec<_> = candidates.into_iter().collect();

    for is_dir in [true, false] {
        if let Some(index) = candidates.iter().position(|candidate| {
            candidate.is_dir == is_dir && candidate.name.to_lowercase().starts_with(&query)
        }) {
            return Some(index);
        }
        if let Some(index) = candidates.iter().position(|candidate| {
            candidate.is_dir == is_dir && candidate.name.to_lowercase().contains(&query)
        }) {
            return Some(index);
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct TypeAheadState {
    buffer: String,
    last_keystroke: Option<Instant>,
}

impl TypeAheadState {
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.last_keystroke = None;
    }

    pub fn push(&mut self, c: char, now: Instant) {
        if self
            .last_keystroke
            .is_some_and(|last| now.saturating_duration_since(last) > TYPE_AHEAD_TIMEOUT)
        {
            self.buffer.clear();
        }
        self.buffer.push(c);
        self.last_keystroke = Some(now);
    }

    pub fn pop(&mut self, now: Instant) {
        self.buffer.pop();
        self.last_keystroke = (!self.buffer.is_empty()).then_some(now);
    }

    pub fn is_active_at(&self, now: Instant) -> bool {
        !self.buffer.is_empty()
            && self
                .last_keystroke
                .is_some_and(|last| now.saturating_duration_since(last) <= TYPE_AHEAD_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_prefix_precedes_file_prefix() {
        let items = [
            TypeAheadCandidate { name: "library.db", is_dir: false },
            TypeAheadCandidate { name: "library", is_dir: true },
        ];
        assert_eq!(first_type_ahead_match(items, "libr"), Some(1));
    }

    #[test]
    fn directory_substring_precedes_file_prefix() {
        let items = [
            TypeAheadCandidate { name: "library.flac", is_dir: false },
            TypeAheadCandidate { name: "my-library", is_dir: true },
        ];
        assert_eq!(first_type_ahead_match(items, "libr"), Some(1));
    }

    #[test]
    fn prefix_precedes_substring_within_kind() {
        let items = [
            TypeAheadCandidate { name: "my-library", is_dir: true },
            TypeAheadCandidate { name: "library", is_dir: true },
        ];
        assert_eq!(first_type_ahead_match(items, "libr"), Some(1));
    }

    #[test]
    fn failed_match_returns_none() {
        let items = [TypeAheadCandidate { name: "music", is_dir: true }];
        assert_eq!(first_type_ahead_match(items, "video"), None);
    }
}
