use std::path::Path;

/// File visibility filter used by the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePickerFilter {
    Images,
    Audio,
    All,
    Custom { label: String, extensions: Vec<String> },
}

impl FilePickerFilter {
    /// Returns true when `path` should be visible for this filter. Directories
    /// are always visible so users can navigate to a matching descendant.
    pub fn accepts_path(&self, path: &Path, is_dir: bool) -> bool {
        if is_dir {
            return true;
        }
        match self {
            Self::All => true,
            Self::Images => extension_matches(
                path,
                SUPPORTED_PREVIEW_IMAGE_EXTENSIONS,
            ),
            Self::Audio => extension_matches(
                path,
                &[
                    "flac", "wav", "aiff", "aif", "wv", "mp3", "m4a", "aac", "ogg",
                    "opus", "dsf", "dff",
                ],
            ),
            Self::Custom { extensions, .. } => path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| extensions.iter().any(|candidate| candidate.eq_ignore_ascii_case(ext)))
                .unwrap_or(false),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Images => "Images".to_string(),
            Self::Audio => "Audio".to_string(),
            Self::All => "All files".to_string(),
            Self::Custom { label, extensions } if label.is_empty() && extensions.is_empty() => {
                "Custom filter".to_string()
            }
            Self::Custom { label, extensions } if label.is_empty() => {
                format!("Custom (*.{})", extensions.join(", *."))
            }
            Self::Custom { label, .. } => label.clone(),
        }
    }
}

impl Default for FilePickerFilter {
    fn default() -> Self {
        Self::All
    }
}

pub(crate) const SUPPORTED_PREVIEW_IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp"];

pub(crate) fn is_supported_preview_image_extension(path: &Path) -> bool {
    extension_matches(path, SUPPORTED_PREVIEW_IMAGE_EXTENSIONS)
}

fn extension_matches(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| extensions.iter().any(|candidate| candidate.eq_ignore_ascii_case(ext)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::FilePickerFilter;
    use std::path::Path;

    #[test]
    fn image_filter_accepts_images_and_directories() {
        let filter = FilePickerFilter::Images;
        assert!(filter.accepts_path(Path::new("cover.JPG"), false));
        assert!(filter.accepts_path(Path::new("folder"), true));
        assert!(!filter.accepts_path(Path::new("notes.txt"), false));
        assert!(!filter.accepts_path(Path::new("scan.tiff"), false));
        assert!(!filter.accepts_path(Path::new("scan.tif"), false));
    }
}
