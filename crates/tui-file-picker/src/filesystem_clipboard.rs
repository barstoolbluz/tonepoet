//! Shared in-memory filesystem clipboard model.

use crate::FilePickerClipboardMode;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemClipboard {
    mode: FilePickerClipboardMode,
    paths: Vec<PathBuf>,
}

impl FilesystemClipboard {
    pub fn new<I>(mode: FilePickerClipboardMode, paths: I) -> Option<Self>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        // Keep only independent roots. A parent path subsumes any selected
        // descendants; retaining both would make a cut/delete sequence depend
        // on iteration order after the parent has moved or disappeared.
        let mut normalized: Vec<PathBuf> = Vec::new();
        for path in paths {
            if normalized.iter().any(|existing| path.starts_with(existing)) {
                continue;
            }
            normalized.retain(|existing| !existing.starts_with(&path));
            normalized.push(path);
        }
        (!normalized.is_empty()).then_some(Self { mode, paths: normalized })
    }

    pub fn mode(&self) -> FilePickerClipboardMode {
        self.mode
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|candidate| candidate == path)
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Plain-text projection mirrored to the host clipboard.
    ///
    /// The in-process clipboard remains authoritative for copy/move semantics;
    /// this projection is intentionally portable and lossless enough for users
    /// to paste the selected paths into a shell, editor, or file manager.
    pub fn text_projection(&self) -> String {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Remap a currently viewed path after a successful cut/paste operation.
///
/// `destinations` must be in the same order as [`FilesystemClipboard::paths`],
/// as guaranteed by `paste_filesystem_clipboard`. Returns `None` for copy
/// operations or when the viewed path was not inside a moved root.
pub fn remap_path_after_cut(
    current: &Path,
    clipboard: &FilesystemClipboard,
    destinations: &[PathBuf],
) -> Option<PathBuf> {
    if clipboard.mode() != FilePickerClipboardMode::Cut {
        return None;
    }
    clipboard
        .paths()
        .iter()
        .zip(destinations)
        .find_map(|(source, destination)| {
            current
                .strip_prefix(source)
                .ok()
                .map(|suffix| destination.join(suffix))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_selection_subsumes_descendants_regardless_of_input_order() {
        let root = PathBuf::from("/music/album");
        let child = root.join("disc-1").join("track.flac");

        let parent_last = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![child.clone(), root.clone()],
        )
        .expect("clipboard");
        assert_eq!(parent_last.paths(), &[root.clone()]);

        let parent_first = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![root.clone(), child],
        )
        .expect("clipboard");
        assert_eq!(parent_first.paths(), &[root]);
    }

    #[test]
    fn remaps_viewed_descendant_after_cut() {
        let source = PathBuf::from("/music/album");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let destination = PathBuf::from("/archive/album");

        assert_eq!(
            remap_path_after_cut(
                &source.join("disc-1"),
                &clipboard,
                std::slice::from_ref(&destination),
            ),
            Some(destination.join("disc-1"))
        );
    }

    #[test]
    fn copy_never_remaps_viewed_path() {
        let source = PathBuf::from("/music/album");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Copy,
            vec![source.clone()],
        )
        .expect("clipboard");
        assert_eq!(
            remap_path_after_cut(
                &source,
                &clipboard,
                &[PathBuf::from("/archive/album")],
            ),
            None
        );
    }

    #[test]
    fn text_projection_preserves_clipboard_order_and_uses_newline_delimiters() {
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Copy,
            vec![
                PathBuf::from("/music/disc 1/01.flac"),
                PathBuf::from("/music/disc 2/02.flac"),
            ],
        )
        .expect("clipboard");

        assert_eq!(
            clipboard.text_projection(),
            "/music/disc 1/01.flac\n/music/disc 2/02.flac"
        );
    }
}
