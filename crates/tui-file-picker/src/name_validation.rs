//! Shared leaf-level validation for inline filesystem names.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameValidationError {
    Empty,
    DotComponent,
    PathSeparator,
    MultipleComponents,
    Nul,
}

impl NameValidationError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "name cannot be empty",
            Self::DotComponent => "name cannot be . or ..",
            Self::PathSeparator => "name cannot contain path separators",
            Self::MultipleComponents => "name must contain exactly one path component",
            Self::Nul => "name cannot contain NUL",
        }
    }
}

pub fn validate_file_name(name: &str) -> Result<&str, NameValidationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(NameValidationError::Empty);
    }
    if trimmed == "." || trimmed == ".." {
        return Err(NameValidationError::DotComponent);
    }
    if trimmed.contains('\0') {
        return Err(NameValidationError::Nul);
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(NameValidationError::PathSeparator);
    }
    if Path::new(trimmed).components().count() != 1 {
        return Err(NameValidationError::MultipleComponents);
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_unicode_name() {
        assert_eq!(validate_file_name("日本語.flac"), Ok("日本語.flac"));
    }

    #[test]
    fn rejects_path_components() {
        assert_eq!(validate_file_name("a/b"), Err(NameValidationError::PathSeparator));
        assert_eq!(validate_file_name(".."), Err(NameValidationError::DotComponent));
    }
}
